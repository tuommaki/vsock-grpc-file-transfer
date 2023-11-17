use std::path::{Path, PathBuf};

use anyhow::Result;
use tokio::{io::AsyncReadExt, sync::mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_vsock::VsockListener;
use tonic::{transport::Server, Request, Response, Status};
use vsock::get_local_cid;

pub mod grpc {
    tonic::include_proto!("demo_service");
}

use grpc::{
    demo_service_server::{DemoService, DemoServiceServer},
    FileRequest, FileResponse,
};

use crate::grpc::file_response;

const DATA_STREAM_CHUNK_SIZE: usize = 4096;

#[derive(Default)]
pub struct FileServer {
    data_dir: String,
}

#[tonic::async_trait]
impl DemoService for FileServer {
    type GetFileStream = ReceiverStream<Result<FileResponse, Status>>;
    async fn get_file(
        &self,
        request: Request<FileRequest>,
    ) -> Result<Response<Self::GetFileStream>, Status> {
        println!("request for file: {:?}", request);

        let req = request.into_inner();

        let mut file = self.open_file(&req.path).await.expect("open file");

        let (tx, rx) = mpsc::channel(4);
        tokio::spawn({
            async move {
                let mut buf: [u8; DATA_STREAM_CHUNK_SIZE] = [0; DATA_STREAM_CHUNK_SIZE];

                loop {
                    match file.read(&mut buf).await {
                        Ok(0) => return Ok(()),
                        Ok(n) => {
                            if let Err(e) = tx
                                .send(Ok(grpc::FileResponse {
                                    result: Some(file_response::Result::Chunk(grpc::FileChunk {
                                        data: buf[..n].to_vec(),
                                    })),
                                }))
                                .await
                            {
                                println!("send {} bytes from file {}: {}", n, &req.path, &e);
                                break;
                            }
                        }
                        Err(e) => return Err(e),
                    }
                }

                Ok(())
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

impl FileServer {
    fn new(data_dir: String) -> Self {
        FileServer { data_dir }
    }

    async fn open_file(&self, path: &str) -> Result<tokio::io::BufReader<tokio::fs::File>> {
        let file_name = Path::new(path).file_name().unwrap();
        let path = PathBuf::new().join(&self.data_dir).join(file_name);
        println!("path: {:#?}", &path);
        let fd = tokio::fs::File::open(path).await?;
        Ok(tokio::io::BufReader::new(fd))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let file_server = FileServer::new("testdata".to_string());
    let cid = get_local_cid().expect("get local CID");
    let listener = VsockListener::bind(cid, 8080).expect("bind");

    Server::builder()
        .add_service(DemoServiceServer::new(file_server))
        .serve_with_incoming(listener.incoming())
        .await?;

    Ok(())
}
