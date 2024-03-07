use std::path::{Path, PathBuf};

use anyhow::Result;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};
use tokio_stream::wrappers::ReceiverStream;
use tokio_vsock::VsockListener;
use tonic::{transport::Server, Code, Request, Response, Status, Streaming};
use vsock::get_local_cid;

pub mod grpc {
    tonic::include_proto!("demo_service");
}

use grpc::{
    demo_service_server::{DemoService, DemoServiceServer},
    Data, EmptyResponse, FileChunk, FileData, FileMetadata, FileRequest, FileResponse,
    GenericResponse, PingRequest, PingResponse,
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

    async fn ping(&self, request: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        let req = request.into_inner();
        Ok(Response::new(PingResponse { msg: req.msg }))
    }

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

    async fn send_data(
        &self,
        request: Request<Streaming<Data>>,
    ) -> Result<Response<EmptyResponse>, Status> {
        let mut stream = request.into_inner();

        let mut n_bytes = 0;

        while let Ok(Some(Data { data })) = stream.message().await {
            n_bytes += data.len();
        }

        println!("received {n_bytes} bytes");

        Ok(Response::new(EmptyResponse {}))
    }

    async fn submit_file(
        &self,
        request: Request<Streaming<FileData>>,
    ) -> Result<Response<GenericResponse>, Status> {
        let mut stream = request.into_inner();
        let mut file: Option<tokio::io::BufWriter<tokio::fs::File>> = None;

        while let Ok(Some(grpc::FileData { result: data })) = stream.message().await {
            match data {
                Some(grpc::file_data::Result::Metadata(FileMetadata { path })) => {
                    let mut path = Path::new(&path);
                    if path.is_absolute() {
                        path = match path.strip_prefix("/") {
                            Ok(path) => path,
                            Err(err) => {
                                println!("failed to strip '/' prefix from file path: {}", err);
                                return Err(Status::new(
                                    Code::Internal,
                                    "failed to strip '/' prefix from file path".to_string(),
                                ));
                            }
                        };
                    }

                    // Ensure any necessary subdirectories exists.
                    if let Some(parent) = path.parent() {
                        tokio::fs::create_dir_all(parent)
                            .await
                            .expect("task file mkdir");
                    }

                    let fd = tokio::fs::File::create(path.with_file_name("received")).await?;
                    file = Some(tokio::io::BufWriter::new(fd));
                }
                Some(grpc::file_data::Result::Chunk(FileChunk { data })) => match file.as_mut() {
                    Some(fd) => {
                        if let Err(err) = fd.write_all(data.as_slice()).await {
                            println!("error while writing to file: {}", err);
                            return Err(Status::new(
                                Code::Internal,
                                "failed to write file".to_string(),
                            ));
                        } else {
                            println!("{} bytes received & written to file", data.len());
                        }
                    }
                    None => {
                        println!("received None from client on submit_file stream");
                        return Err(Status::new(
                            Code::InvalidArgument,
                            "file data sent before metadata; aborting".to_string(),
                        ));
                    }
                },
                Some(grpc::file_data::Result::Error(code)) => {
                    println!("error from client: {code}");
                    return Err(Status::new(
                        Code::Aborted,
                        format!("file transfer aborted by client; error code: {code}"),
                    ));
                }
                None => {
                    println!("FileData message with None as a body");
                    return Err(Status::new(
                        Code::InvalidArgument,
                        "file data sent without body".to_string(),
                    ));
                }
            }
        }

        if file.is_some() {
            if let Err(err) = file.unwrap().flush().await {
                println!("failed to flush file: {}", err);
                return Err(Status::new(
                    Code::Internal,
                    "failed to flush file writes".to_string(),
                ));
            };
        }

        Ok(Response::new(GenericResponse {
            success: true,
            message: String::from("file received"),
        }))
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
