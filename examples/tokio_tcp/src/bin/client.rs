use tokio::time;
use std::time::Duration;

use toy_rpc::client::{Client, Call};

#[tokio::main]
async fn main() {
    run().await;
    println!("After run");
}

async fn run() {
    env_logger::init();

    let addr = "127.0.0.1:23333";
    let client = Client::dial(addr).await.unwrap();

    let call: Call<i32> = client.call("Echo.echo_i32", 13i32);
    let reply = call.await;
    println!("{:?}", reply);

    let reply: Result<i32, _> = client.call("Echo.echo_i32", 1313i32).await;
    println!("{:?}", reply);

    println!("Calling finite loop");
    let call: Call<()> = client.call("Echo.finite_loop", ());
    time::sleep(Duration::from_secs(2)).await;
    println!("Calling cancellation");
    call.cancel();

    let call: Call<i32> = client.call("Echo.echo_i32", 13i32);
    let reply = call.await;
    println!("{:?}", reply);

    // let reply: Result<i32, _> = client.call("Echo.echo_i32", 1313i32).await;
    // println!("{:?}", reply);
}