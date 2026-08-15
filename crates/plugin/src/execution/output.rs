use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
};

use tokio::sync::Notify;

pub const OUTPUT_CHUNK_BYTES: usize = 16 * 1024;
pub const OUTPUT_BUFFER_BYTES: usize = 256 * 1024;

pub struct OutputSink {
    shared: Arc<Shared>,
}

pub struct OutputStream {
    shared: Arc<Shared>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmitResult {
    Written,
    Disconnected,
    Cancelled,
    Stopped,
    Finished,
}

struct Shared {
    state: Mutex<State>,
    space_available: Condvar,
    data_available: Notify,
    producers: AtomicUsize,
}

#[derive(Default)]
struct State {
    ready: VecDeque<String>,
    pending: String,
    queued_bytes: usize,
    disconnected: bool,
    cancelled: bool,
    stopped: bool,
    finished: bool,
}

pub fn channel() -> (OutputSink, OutputStream) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            pending: String::new(),
            ..State::default()
        }),
        space_available: Condvar::new(),
        data_available: Notify::new(),
        producers: AtomicUsize::new(1),
    });
    (
        OutputSink {
            shared: Arc::clone(&shared),
        },
        OutputStream { shared },
    )
}

impl Clone for OutputSink {
    fn clone(&self) -> Self {
        self.shared.producers.fetch_add(1, Ordering::Relaxed);
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl OutputSink {
    pub fn emit(&self, mut text: &str) -> EmitResult {
        if text.is_empty() {
            return emit_result(&lock(&self.shared.state));
        }

        while !text.is_empty() {
            let mut state = lock(&self.shared.state);

            loop {
                let result = emit_result(&state);

                if result != EmitResult::Written {
                    return result;
                }

                let chunk_space = OUTPUT_CHUNK_BYTES - state.pending.len();
                let buffer_space = OUTPUT_BUFFER_BYTES.saturating_sub(state.queued_bytes);
                let byte_count = utf8_prefix_len(text, chunk_space.min(buffer_space));

                if byte_count != 0 {
                    let fragment = &text[..byte_count];
                    state.pending.push_str(fragment);
                    state.queued_bytes += byte_count;
                    text = &text[byte_count..];

                    if state.pending.len() == OUTPUT_CHUNK_BYTES {
                        publish_pending(&mut state);
                        drop(state);
                        self.shared.data_available.notify_one();
                    }

                    break;
                }

                if !state.pending.is_empty() {
                    publish_pending(&mut state);
                    self.shared.data_available.notify_one();
                    continue;
                }

                state = wait(&self.shared.space_available, state);
            }
        }

        EmitResult::Written
    }

    pub fn flush(&self) -> EmitResult {
        let mut state = lock(&self.shared.state);
        let result = emit_result(&state);

        if result == EmitResult::Written {
            publish_pending(&mut state);
        }

        drop(state);
        self.shared.data_available.notify_one();
        result
    }

    pub fn request_cancel(&self) {
        let mut state = lock(&self.shared.state);
        publish_pending(&mut state);
        state.cancelled = true;
        drop(state);
        self.wake_all();
    }

    #[cfg(test)]
    pub fn cancel_requested(&self) -> bool {
        lock(&self.shared.state).cancelled
    }

    pub fn finish(&self) {
        let mut state = lock(&self.shared.state);
        publish_pending(&mut state);
        state.finished = true;
        drop(state);
        self.wake_all();
    }

    pub fn stop(&self) {
        let mut state = lock(&self.shared.state);
        state.stopped = true;
        state.ready.clear();
        state.pending.clear();
        state.queued_bytes = 0;
        drop(state);
        self.wake_all();
    }

    fn wake_all(&self) {
        self.shared.space_available.notify_all();
        self.shared.data_available.notify_one();
    }

    #[cfg(test)]
    pub(crate) fn queued_bytes(&self) -> usize {
        lock(&self.shared.state).queued_bytes
    }

    #[cfg(test)]
    fn queued_chunks(&self) -> usize {
        let state = lock(&self.shared.state);
        state.ready.len() + usize::from(!state.pending.is_empty())
    }
}

impl Drop for OutputSink {
    fn drop(&mut self) {
        if self.shared.producers.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }

        let mut state = lock(&self.shared.state);

        if emit_result(&state) != EmitResult::Written {
            return;
        }

        state.stopped = true;
        state.ready.clear();
        state.pending.clear();
        state.queued_bytes = 0;
        drop(state);
        self.shared.space_available.notify_all();
        self.shared.data_available.notify_one();
    }
}

impl OutputStream {
    pub async fn next_chunk(&mut self) -> Option<String> {
        loop {
            let notified = self.shared.data_available.notified();
            {
                let mut state = lock(&self.shared.state);

                if let Some(chunk) = state.ready.pop_front() {
                    state.queued_bytes -= chunk.len();
                    drop(state);
                    self.shared.space_available.notify_all();

                    return Some(chunk);
                }

                if state.disconnected || state.cancelled || state.stopped || state.finished {
                    return None;
                }
            }

            notified.await;
        }
    }

    fn disconnect(&self) {
        let mut state = lock(&self.shared.state);
        state.disconnected = true;
        state.ready.clear();
        state.pending.clear();
        state.queued_bytes = 0;
        drop(state);
        self.shared.space_available.notify_all();
        self.shared.data_available.notify_one();
    }
}

impl Drop for OutputStream {
    fn drop(&mut self) {
        self.disconnect();
    }
}

fn utf8_prefix_len(text: &str, limit: usize) -> usize {
    let mut end = limit.min(text.len());

    while end != 0 && !text.is_char_boundary(end) {
        end -= 1;
    }

    end
}

fn publish_pending(state: &mut State) {
    if state.pending.is_empty() {
        return;
    }

    if let Some(chunk) = state.ready.back_mut() {
        let byte_count = utf8_prefix_len(&state.pending, OUTPUT_CHUNK_BYTES - chunk.len());

        if byte_count != 0 {
            chunk.push_str(&state.pending[..byte_count]);
            state.pending.drain(..byte_count);
        }
    }

    if !state.pending.is_empty() {
        let chunk = std::mem::take(&mut state.pending);
        state.ready.push_back(chunk);
    }
}

fn emit_result(state: &State) -> EmitResult {
    if state.disconnected {
        EmitResult::Disconnected
    } else if state.stopped {
        EmitResult::Stopped
    } else if state.cancelled {
        EmitResult::Cancelled
    } else if state.finished {
        EmitResult::Finished
    } else {
        EmitResult::Written
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait<'a, T>(condvar: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    condvar
        .wait(guard)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread, time::Duration};

    use super::*;

    #[tokio::test]
    async fn preserves_bytes_with_bounded_coalesced_chunks() {
        let (sink, mut stream) = channel();

        for _ in 0..100_000 {
            assert_eq!(sink.emit("x"), EmitResult::Written);
        }

        sink.finish();

        assert_eq!(sink.queued_bytes(), 100_000);
        assert_eq!(sink.queued_chunks(), 7);
        let mut output = String::new();

        while let Some(chunk) = stream.next_chunk().await {
            assert!(chunk.len() <= OUTPUT_CHUNK_BYTES);
            output.push_str(&chunk);
        }

        assert_eq!(output, "x".repeat(100_000));
    }

    #[tokio::test]
    async fn splits_only_at_utf8_boundaries() {
        let (sink, mut stream) = channel();
        let text = "界".repeat(OUTPUT_CHUNK_BYTES);
        assert_eq!(sink.emit(&text), EmitResult::Written);
        sink.finish();

        let mut output = String::new();

        while let Some(chunk) = stream.next_chunk().await {
            assert!(chunk.len() <= OUTPUT_CHUNK_BYTES);
            output.push_str(&chunk);
        }

        assert_eq!(output, text);
    }

    #[test]
    fn backpressure_blocks_until_the_consumer_drains_space() {
        let (sink, mut stream) = channel();
        assert_eq!(
            sink.emit(&"x".repeat(OUTPUT_BUFFER_BYTES)),
            EmitResult::Written
        );

        let (completed, completion) = mpsc::channel();
        let producer = sink.clone();
        let writer = thread::spawn(move || {
            completed.send(producer.emit("y")).unwrap();
        });
        assert!(completion.recv_timeout(Duration::from_millis(20)).is_err());

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(runtime.block_on(stream.next_chunk()).is_some());
        assert_eq!(
            completion.recv_timeout(Duration::from_secs(1)).unwrap(),
            EmitResult::Written
        );
        writer.join().unwrap();
    }

    #[test]
    fn cancellation_wakes_a_blocked_producer_without_discarding_queued_output() {
        let (sink, _stream) = channel();
        assert_eq!(
            sink.emit(&"x".repeat(OUTPUT_BUFFER_BYTES)),
            EmitResult::Written
        );

        let (completed, completion) = mpsc::channel();
        let producer = sink.clone();
        let writer = thread::spawn(move || {
            completed.send(producer.emit("y")).unwrap();
        });
        assert!(completion.recv_timeout(Duration::from_millis(20)).is_err());

        sink.request_cancel();
        assert_eq!(
            completion.recv_timeout(Duration::from_secs(1)).unwrap(),
            EmitResult::Cancelled
        );
        assert_eq!(sink.queued_bytes(), OUTPUT_BUFFER_BYTES);
        writer.join().unwrap();
    }

    #[test]
    fn disconnect_discards_queued_and_future_output_without_cancelling() {
        let (sink, stream) = channel();
        assert_eq!(sink.emit("pending"), EmitResult::Written);
        drop(stream);

        assert_eq!(sink.queued_bytes(), 0);
        assert_eq!(sink.emit("later"), EmitResult::Disconnected);
        assert!(!sink.cancel_requested());
    }

    #[tokio::test]
    async fn dropping_a_pending_read_does_not_consume_output() {
        let (sink, mut stream) = channel();
        tokio::select! {
            _ = stream.next_chunk() => panic!("an empty stream should remain pending"),
            _ = tokio::task::yield_now() => {}
        }

        assert_eq!(sink.emit("still here"), EmitResult::Written);
        assert_eq!(sink.flush(), EmitResult::Written);
        assert_eq!(stream.next_chunk().await.as_deref(), Some("still here"));
    }

    #[tokio::test]
    async fn partial_fragments_are_not_messages_until_flushed() {
        let (sink, mut stream) = channel();

        for _ in 0..1_000 {
            assert_eq!(sink.emit("x"), EmitResult::Written);
        }

        assert!(
            tokio::time::timeout(Duration::from_millis(20), stream.next_chunk())
                .await
                .is_err()
        );

        assert_eq!(sink.flush(), EmitResult::Written);
        assert_eq!(stream.next_chunk().await, Some("x".repeat(1_000)));
    }

    #[tokio::test]
    async fn finish_wakes_a_pending_reader() {
        let (sink, mut stream) = channel();
        let reader = tokio::spawn(async move { stream.next_chunk().await });
        tokio::task::yield_now().await;

        sink.finish();
        assert_eq!(reader.await.unwrap(), None);
    }

    #[tokio::test]
    async fn cancellation_drains_published_output_then_ends_the_stream() {
        let (sink, mut stream) = channel();
        assert_eq!(sink.emit("before cancel"), EmitResult::Written);
        sink.request_cancel();

        assert_eq!(stream.next_chunk().await.as_deref(), Some("before cancel"));
        assert_eq!(stream.next_chunk().await, None);
    }

    #[tokio::test]
    async fn dropping_the_last_producer_stops_an_unfinished_stream() {
        let (sink, mut stream) = channel();
        assert_eq!(sink.emit("discarded"), EmitResult::Written);
        drop(sink);

        assert_eq!(stream.next_chunk().await, None);
    }

    #[tokio::test]
    async fn dropping_one_producer_does_not_stop_its_clone() {
        let (sink, mut stream) = channel();
        let remaining = sink.clone();
        drop(sink);

        assert_eq!(remaining.emit("kept"), EmitResult::Written);
        remaining.finish();
        assert_eq!(stream.next_chunk().await.as_deref(), Some("kept"));
        assert_eq!(stream.next_chunk().await, None);
    }

    #[tokio::test]
    async fn dropping_a_finished_producer_preserves_published_output() {
        let (sink, mut stream) = channel();
        assert_eq!(sink.emit("kept"), EmitResult::Written);
        sink.finish();
        drop(sink);

        assert_eq!(stream.next_chunk().await.as_deref(), Some("kept"));
        assert_eq!(stream.next_chunk().await, None);
    }
}
