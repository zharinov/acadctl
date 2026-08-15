use std::io::{self, Write};
use std::thread;

use tokio::sync::{mpsc, oneshot};

#[derive(Clone)]
pub(super) struct PipeWriter {
    sender: mpsc::Sender<PipeWrite>,
}

struct PipeWrite {
    text: String,
    completion: oneshot::Sender<io::Result<()>>,
}

impl PipeWriter {
    pub(super) fn stdout() -> io::Result<Self> {
        Self::spawn(io::stdout(), 1, "acadctl-stdout")
    }

    pub(super) fn stderr() -> io::Result<Self> {
        Self::spawn(io::stderr(), 8, "acadctl-stderr")
    }

    pub(super) fn spawn<W>(mut writer: W, capacity: usize, name: &str) -> io::Result<Self>
    where
        W: Write + Send + 'static,
    {
        let (sender, mut receiver) = mpsc::channel::<PipeWrite>(capacity);
        thread::Builder::new().name(name.into()).spawn(move || {
            while let Some(write) = receiver.blocking_recv() {
                let result = writer
                    .write_all(write.text.as_bytes())
                    .and_then(|()| writer.flush());
                let failed = result.is_err();
                let _ = write.completion.send(result);

                if failed {
                    return;
                }
            }
        })?;
        Ok(Self { sender })
    }

    pub(super) async fn write(&self, text: String) -> io::Result<()> {
        let (completion, result) = oneshot::channel();
        self.sender
            .send(PipeWrite { text, completion })
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "pipe writer stopped"))?;
        result.await.map_err(|_| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "pipe writer stopped without reporting a result",
            )
        })?
    }

    pub(super) fn try_write(&self, text: String) {
        let (completion, _result) = oneshot::channel();
        let _ = self.sender.try_send(PipeWrite { text, completion });
    }
}
