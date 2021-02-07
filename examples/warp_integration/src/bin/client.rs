use toy_rpc::client::Client;
use toy_rpc::error::Error;

use warp_integration::rpc::{BarRequest, BarResponse, FooRequest, FooResponse};

#[tokio::main]
async fn main() {
    pretty_env_logger::init();

    let addr = "ws://127.0.0.1:23333/rpc/";
    let client = Client::dial_http(addr).await.unwrap();

    let args = FooRequest { a: 1, b: 3 };
    let reply: Result<FooResponse, Error> = client.call("foo_service.echo", &args);
    println!("{:?}", reply);

    let reply: Result<FooResponse, Error> = client.async_call("foo_service.increment_a", &args).await;
    println!("{:?}", reply);

    let handle = client.spawn_task("foo_service.increment_b", args);
    let reply: Result<FooResponse, Error> = handle.await.unwrap();
    println!("{:?}", reply);

    // third request, bar echo
    let args = BarRequest {
        content: "bar".to_string(),
    };
    let reply: BarResponse = client.call("bar_service.echo", &args).unwrap();
    println!("{:?}", reply);

    // fourth request, bar exclaim
    let reply: BarResponse = client.async_call("bar_service.exclaim", &args).await.unwrap();
    println!("{:?}", reply);

    // third request, get_counter
    let args = ();
    let handle = client.spawn_task("foo_service.get_counter", args);
    let reply: u32 = handle.await.unwrap().unwrap();
    println!("{:?}", reply);

    client.close().await;
}