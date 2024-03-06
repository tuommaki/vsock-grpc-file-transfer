use std::path::Path;

use anyhow::Result;
use grpc::{demo_service_client::DemoServiceClient, FileChunk, FileData, FileMetadata};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_stream::StreamExt;
use tokio_vsock::VsockStream;
use tonic::{
    transport::{Channel, Endpoint, Uri},
    Request,
};
use tower::service_fn;
use vsock::VMADDR_CID_HOST;

pub mod grpc {
    tonic::include_proto!("demo_service");
}

const DATA_STREAM_CHUNK_SIZE: usize = 4096;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let channel = Endpoint::try_from(format!("http://[::]:{}", 8080))?
        .connect_with_connector(service_fn(move |_: Uri| {
            // Connect to a VSOCK server
            VsockStream::connect(VMADDR_CID_HOST, 8080)
        }))
        .await?;

    let mut client = DemoServiceClient::new(channel);

    let _response = download_file(&mut client, "test_file_1").await?;

    submit_file(&mut client, "/workspace/test_file_1").await?;

    Ok(())
}

/// download_file asks gRPC server for file with a `name` and writes it to
/// `workspace`.
async fn download_file(client: &mut DemoServiceClient<Channel>, name: &str) -> Result<String> {
    let file_req = grpc::FileRequest {
        path: name.to_string(),
    };

    let path = match Path::new("/workspace")
        .join(name)
        .into_os_string()
        .into_string()
    {
        Ok(path) => path,
        Err(e) => panic!("failed to construct path for a file to write: {:?}", e),
    };

    let mut stream = client.get_file(file_req).await?.into_inner();
    let out_file = tokio::fs::File::create(path.clone()).await?;
    let mut writer = tokio::io::BufWriter::new(out_file);

    let mut total_bytes = 0;

    while let Some(Ok(grpc::FileResponse { result: resp })) = stream.next().await {
        match resp {
            Some(grpc::file_response::Result::Chunk(file_chunk)) => {
                total_bytes += file_chunk.data.len();
                writer.write_all(file_chunk.data.as_ref()).await?;
            }
            Some(grpc::file_response::Result::Error(err)) => {
                panic!("error while fetching file {}: {}", name, err)
            }
            None => {
                println!("stream broken");
                break;
            }
        }
    }

    writer.flush().await?;

    println!("downloaded {} bytes for {}", &total_bytes, &name);

    Ok(path)
}

async fn send_data(client: &mut DemoServiceClient<Channel>, n_bytes: usize) -> Result<()> {
    let outbound = async_stream::stream! {
        let mut written = 0;

        while written <= n_bytes {
            let bytes_to_write = std::cmp::min(n_bytes-written, DATA_STREAM_CHUNK_SIZE);
            let bytes = (0..bytes_to_write).map(|_| { rand::random::<u8>() }).collect();

            let data = grpc::Data{
                data: bytes,
            };

            written += bytes_to_write;

            yield data;
        }
    };

    if let Err(err) = client.send_data(Request::new(outbound)).await {
        println!("failed to submit data: {}", err);
    }

    Ok(())
}

async fn submit_file(client: &mut DemoServiceClient<Channel>, file_path: &str) -> Result<()> {
    let file_path = file_path.to_string();
    let outbound = async_stream::stream! {
        let fd = match tokio::fs::File::open(&file_path).await {
            Ok(fd) => fd,
            Err(err) => {
                println!("failed to open file {file_path}: {}", err);
                return;
            }
        };

        let mut file = tokio::io::BufReader::new(fd);

        let metadata = FileData{ result: Some(grpc::file_data::Result::Metadata(FileMetadata {
            path: file_path.to_string(),
        }))};

        yield metadata;

        let mut buf: [u8; DATA_STREAM_CHUNK_SIZE] = [0; DATA_STREAM_CHUNK_SIZE];
        loop {
            match file.read(&mut buf).await {
                Ok(0) => return,
                Ok(n) => {
                    yield FileData{ result: Some(grpc::file_data::Result::Chunk(FileChunk{ data: buf[..n].to_vec() }))};
                },
                Err(err) => {
                    println!("failed to read file: {}", err);
                    yield FileData{ result: Some(grpc::file_data::Result::Error(1))};
                }
            }
        }
    };

    if let Err(err) = client.submit_file(Request::new(outbound)).await {
        println!("failed to submit file: {}", err);
    }

    Ok(())
}
