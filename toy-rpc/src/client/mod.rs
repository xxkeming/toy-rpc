use cfg_if::cfg_if;
use flume::{Receiver, Sender};
use futures::{lock::Mutex, Future, FutureExt};
use pin_project::pin_project;
use std::{
    collections::HashMap,
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use crate::message::CANCELLATION_TOKEN;
use crate::{
    codec::split::{ClientCodecRead, ClientCodecWrite},
    message::{
        AtomicMessageId, MessageId, RequestBody, RequestHeader, ResponseHeader, ResponseResult,
        CANCELLATION_TOKEN_DELIM,
    },
    Error,
};

cfg_if! {
    if #[cfg(any(
        all(
            feature = "serde_bincode",
            not(feature = "serde_json"),
            not(feature = "serde_cbor"),
            not(feature = "serde_rmp"),
        ),
        all(
            feature = "serde_cbor",
            not(feature = "serde_json"),
            not(feature = "serde_bincode"),
            not(feature = "serde_rmp"),
        ),
        all(
            feature = "serde_json",
            not(feature = "serde_bincode"),
            not(feature = "serde_cbor"),
            not(feature = "serde_rmp"),
        ),
        all(
            feature = "serde_rmp",
            not(feature = "serde_cbor"),
            not(feature = "serde_json"),
            not(feature = "serde_bincode"),
        )
    ))] {
        use crate::codec::DefaultCodec;
    }
}

cfg_if! {
    if #[cfg(feature = "async_std_runtime")] {
        use futures::channel::oneshot;
        use futures::select;

        mod async_std;
        pub use crate::client::async_std::Call;

    } else if #[cfg(feature = "tokio_runtime")] {
        use ::tokio::sync::oneshot;
        use ::tokio::select;

        mod tokio;
        pub use crate::client_new::tokio::Call;
    }
}

/// Type state for creating `Client`
pub struct NotConnected {}
/// Type state for creating `Client`
pub struct Connected {}

// There will be a dedicated task for reading and writing, so there should be no
// contention across tasks or threads
// type Codec = Box<dyn ClientCodec>;
type ResponseMap = HashMap<MessageId, oneshot::Sender<ResponseResult>>;

/// RPC client
///
pub struct Client<Mode> {
    count: AtomicMessageId,
    pending: Arc<Mutex<ResponseMap>>,

    // new request will be sent over this channel
    requests: Sender<(RequestHeader, RequestBody)>,

    // both reader and writer tasks should return nothingcliente handles will be used to drop the tasks
    // The Drop trait should be impled when tokio or async_std runtime is enabled
    reader_stop: Sender<()>,
    writer_stop: Sender<()>,

    marker: PhantomData<Mode>,
}

// seems like it still works even without this impl
impl<Mode> Drop for Client<Mode> {
    fn drop(&mut self) {
        log::debug!("Dropping client");

        if self.reader_stop.send(()).is_err() {
            log::error!("Failed to send stop signal to reader loop")
        }
        if self.writer_stop.send(()).is_err() {
            log::error!("Failed to send stop signal to writer loop")
        }
    }
}

pub(crate) async fn reader_loop(
    mut reader: impl ClientCodecRead,
    pending: Arc<Mutex<ResponseMap>>,
    stop: Receiver<()>,
) {
    loop {
        select! {
            _ = stop.recv_async().fuse() => {
                return ()
            },
            res = read_once(&mut reader, &pending).fuse() => {
                match res {
                    Ok(_) => {}
                    Err(err) => log::error!("{:?}", err),
                }
            }
        }
    }
}

async fn read_once(
    reader: &mut impl ClientCodecRead,
    pending: &Arc<Mutex<ResponseMap>>,
) -> Result<(), Error> {
    if let Some(header) = reader.read_response_header().await {
        // [1] destructure header
        let ResponseHeader { id, is_error } = header?;
        // [2] get resposne body
        let deserialzer =
            reader
                .read_response_body()
                .await
                .ok_or(Error::IoError(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Unexpected EOF reading response body",
                )))?;
        let deserializer = deserialzer?;

        let res = match is_error {
            false => Ok(deserializer),
            true => Err(deserializer),
        };

        // [3] send back response
        {
            let mut _pending = pending.lock().await;
            if let Some(done_sender) = _pending.remove(&id) {
                done_sender.send(res).map_err(|_| {
                    Error::Internal(
                        "InternalError: client failed to send response over channel".into(),
                    )
                })?;
            }
        }
    }
    Ok(())
}

pub(crate) async fn writer_loop(
    mut writer: impl ClientCodecWrite,
    requests: Receiver<(RequestHeader, RequestBody)>,
    stop: Receiver<()>,
) {
    loop {
        select! {
            _ = stop.recv_async().fuse() => {
                // finish sending all requests available before dropping
                for (header, body) in requests.drain().into_iter() {
                    match writer.write_request(header, &body).await {
                        Ok(_) => { },
                        Err(err) => log::error!("{:?}", err)
                    }
                }
                return ()
            },
            res = write_once(&mut writer, &requests).fuse() => {
                match res {
                    Ok(_) => {}
                    Err(err) => log::error!("{:?}", err),
                }
            }
        }
    }
}

async fn write_once(
    writer: &mut impl ClientCodecWrite,
    request: &Receiver<(RequestHeader, RequestBody)>,
) -> Result<(), Error> {
    if let Ok(req) = request.recv_async().await {
        let (header, body) = req;
        println!("{:?}", &header);
        writer.write_request(header, &body).await?;
    }
    Ok(())
}

async fn handle_call<Res>(
    pending: Arc<Mutex<ResponseMap>>,
    header: RequestHeader,
    body: RequestBody,
    request_tx: Sender<(RequestHeader, RequestBody)>,
    cancel: oneshot::Receiver<MessageId>,
    done: oneshot::Sender<Result<Res, Error>>,
) -> Result<(), Error>
where
    Res: serde::de::DeserializeOwned + Send,
{
    let id = header.id.clone();
    request_tx.send_async((header, body)).await?;

    let (resp_tx, resp_rx) = oneshot::channel();

    // insert done channel to ResponseMap
    {
        let mut _pending = pending.lock().await;
        _pending.insert(id, resp_tx);
    }

    select! {
        res = cancel.fuse() => {
            if let Ok(id) = res {
                let header = RequestHeader {
                    id,
                    service_method: CANCELLATION_TOKEN.into(),
                };
                let body: String = 
                    format!("{}{}{}", CANCELLATION_TOKEN, CANCELLATION_TOKEN_DELIM, id);
                let body = Box::new(body) as RequestBody;
                request_tx.send_async((header, body)).await?;
            }
        },
        res = handle_response(resp_rx, done).fuse() => { 
            match res {
                Ok(_) => { },
                Err(err) => log::error!("{:?}", err)
            }
        }
    };

    Ok(())
}

async fn handle_response<Res>(
    response: oneshot::Receiver<ResponseResult>,
    done: oneshot::Sender<Result<Res, Error>>
) -> Result<(), Error> 
where 
    Res: serde::de::DeserializeOwned +Send
{
    let val = response.await
        // cancellation of the oneshot channel is not intended 
        // and thus should be considered as an InternalError
        .map_err(|err| Error::Internal(Box::new(err)))?;
    let res = match val {
        Ok(mut resp_body) => erased_serde::deserialize(&mut resp_body)
            .map_err(|err| Error::ParseError(Box::new(err))),
        Err(mut err_body) => erased_serde::deserialize(&mut err_body)
            .map_or_else(
                |err| Err(Error::ParseError(Box::new(err))), 
                |msg| Err(Error::from_err_msg(msg))), // handles error msg sent from server
    };

    done.send(res)
        .map_err(|_| Error::Internal("Failed to send over done channel".into()))?;
    Ok(())
}