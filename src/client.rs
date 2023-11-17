use std::path::Path;

use anyhow::Result;
use grpc::demo_service_client::DemoServiceClient;
use tokio::io::AsyncWriteExt;
use tokio_stream::StreamExt;
use tokio_vsock::VsockStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;
use vsock::VMADDR_CID_HOST;

pub mod grpc {
    tonic::include_proto!("demo_service");
}

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
