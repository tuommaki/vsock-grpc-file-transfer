use std::{path::Path, sync::Arc, time::Duration};

use anyhow::Result;
use grpc::{demo_service_client::DemoServiceClient, FileChunk, FileData, FileMetadata};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
};
use tokio_stream::StreamExt;
use tokio_vsock::VsockStream;
use tonic::{
    transport::{Channel, Endpoint, Uri},
    Request,
};
use tower::service_fn;
use vsock::VMADDR_CID_HOST;

use crate::grpc::PingRequest;

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

    let client = Arc::new(Mutex::new(DemoServiceClient::new(channel)));

    // Dummy vector from heap, where first messenger task writes numbers on
    // every iteration.
    let mut alloc: Vec<u64> = vec![];

    // Generate some load for scheduler.
    let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
    let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
    let (tx3, mut rx3) = tokio::sync::mpsc::unbounded_channel();
    let (tx4, mut rx4) = tokio::sync::mpsc::unbounded_channel::<&str>();

    tokio::spawn({
        let client = client.clone();
        async move {
            loop {
                tx1.send("message from first").expect("tx1.send");
                if let Some(msg) = rx4.recv().await {
                    let res = client
                        .lock()
                        .await
                        .ping(Request::new(PingRequest {
                            msg: msg.as_bytes().to_vec(),
                        }))
                        .await
                        .expect("task #1: client.ping()")
                        .into_inner();

                    if res.msg != msg.as_bytes().to_vec() {
                        println!("ping request and response differ");
                        break;
                    }

                    alloc.push(42);
                } else {
                    println!("first task: no more messages from rx4");
                    break;
                };
            }
        }
    });

    tokio::spawn({
        let client = client.clone();
        async move {
            while let Some(msg) = rx1.recv().await {
                let res = client
                    .lock()
                    .await
                    .ping(Request::new(PingRequest {
                        msg: msg.as_bytes().to_vec(),
                    }))
                    .await
                    .expect("task #2: client.ping()")
                    .into_inner();

                if res.msg != msg.as_bytes().to_vec() {
                    println!("ping request and response differ");
                    break;
                }

                tx2.send("message from second").expect("tx2.send");
            }
        }
    });

    tokio::spawn({
        let client = client.clone();
        async move {
            while let Some(msg) = rx2.recv().await {
                let res = client
                    .lock()
                    .await
                    .ping(Request::new(PingRequest {
                        msg: msg.as_bytes().to_vec(),
                    }))
                    .await
                    .expect("task #3: client.ping()")
                    .into_inner();

                if res.msg != msg.as_bytes().to_vec() {
                    println!("ping request and response differ");
                    break;
                }

                tx3.send("message from third").expect("tx3.send");
            }
        }
    });

    tokio::spawn({
        let client = client.clone();
        async move {
            while let Some(msg) = rx3.recv().await {
                let res = client
                    .lock()
                    .await
                    .ping(Request::new(PingRequest {
                        msg: msg.as_bytes().to_vec(),
                    }))
                    .await
                    .expect("task #4: client.ping()")
                    .into_inner();

                if res.msg != msg.as_bytes().to_vec() {
                    println!("ping request and response differ");
                    break;
                }

                tx4.send("message from fourth").expect("tx4.send");
            }
        }
    });

    println!("sleep for 5 secs");
    tokio::time::sleep(Duration::from_secs(5)).await;

    download_file(client.clone(), "test_file_1").await?;

    println!("sleep for 5 secs");
    tokio::time::sleep(Duration::from_secs(5)).await;

    submit_file(client.clone(), "/workspace/test_file_1").await?;

    Ok(())
}

/// download_file asks gRPC server for file with a `name` and writes it to
/// `workspace`.
async fn download_file(
    client: Arc<Mutex<DemoServiceClient<Channel>>>,
    name: &str,
) -> Result<String> {
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

    let mut client = client.lock().await;
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

async fn send_data(client: Arc<Mutex<DemoServiceClient<Channel>>>, n_bytes: usize) -> Result<()> {
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

    let mut client = client.lock().await;
    if let Err(err) = client.send_data(Request::new(outbound)).await {
        println!("failed to submit data: {}", err);
    }

    Ok(())
}

async fn submit_file(
    client: Arc<Mutex<DemoServiceClient<Channel>>>,
    file_path: &str,
) -> Result<()> {
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

    let mut client = client.lock().await;
    if let Err(err) = client.submit_file(Request::new(outbound)).await {
        println!("failed to submit file: {}", err);
    }

    Ok(())
}
