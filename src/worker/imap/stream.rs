//! Bound a silent IMAP connection, including reads while draining FETCH
//! literals. A command-level timeout alone misses the response streams.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::Sleep;

use super::namespace::NamespaceFilter;
use crate::i18n::i18n;

const IO_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub(in crate::worker) struct ImapIo<S> {
    inner: S,
    timeout: Duration,
    read_timer: Option<Pin<Box<Sleep>>>,
    write_timer: Option<Pin<Box<Sleep>>>,
    idle: bool,
    failed: bool,
    selected_folder: Option<String>,
    /// While NAMESPACE is out (#554), what arrives is read through this.
    namespace: Option<NamespaceFilter>,
    /// Bytes read and let through, not yet handed to async-imap.
    passing: Vec<u8>,
}

impl<S> ImapIo<S> {
    pub(in crate::worker) fn new(inner: S) -> Self {
        Self {
            inner,
            timeout: IO_TIMEOUT,
            read_timer: None,
            write_timer: None,
            idle: false,
            failed: false,
            selected_folder: None,
            namespace: None,
            passing: Vec::new(),
        }
    }

    /// Keep a `* NAMESPACE` reply back from async-imap, whose parser would
    /// fail the session on it, until [`Self::release_namespace`].
    pub(in crate::worker) fn hold_namespace(&mut self) {
        self.namespace = Some(NamespaceFilter::default());
    }

    /// Stop holding, and hand over the NAMESPACE reply if one came.
    pub(in crate::worker) fn release_namespace(&mut self) -> Option<Vec<u8>> {
        let filter = self.namespace.take()?;
        filter.finish(&mut self.passing)
    }

    // IDLE has its own wait, init and DONE deadlines. Silence while waiting
    // for new mail is expected; silence during any other read is not.
    pub(in crate::worker) fn set_idle(&mut self, idle: bool) {
        self.idle = idle;
        self.read_timer = None;
    }

    pub(in crate::worker) fn is_selected(&self, path: &str) -> bool {
        !self.failed && self.selected_folder.as_deref() == Some(path)
    }

    pub(in crate::worker) fn set_selected_folder(&mut self, path: Option<&str>) {
        self.selected_folder = path.map(str::to_owned);
    }

    fn timed_out() -> io::Error {
        io::Error::new(
            io::ErrorKind::TimedOut,
            i18n("IMAP connection stopped making progress"),
        )
    }
}

fn elapsed(timer: &mut Option<Pin<Box<Sleep>>>, timeout: Duration, cx: &mut Context<'_>) -> bool {
    timer
        .get_or_insert_with(|| Box::pin(tokio::time::sleep(timeout)))
        .as_mut()
        .poll(cx)
        .is_ready()
}

impl<S: AsyncRead + Unpin> ImapIo<S> {
    fn poll_wire(&mut self, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        if self.failed {
            return Poll::Ready(Err(Self::timed_out()));
        }
        let before = buf.filled().len();
        match Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Ready(result) => {
                if buf.filled().len() > before {
                    self.read_timer = None;
                }
                Poll::Ready(result)
            }
            Poll::Pending => {
                let timeout = self.timeout;
                if !self.idle && elapsed(&mut self.read_timer, timeout, cx) {
                    // A late reply must not be consumed as the next command's
                    // response. Poison both directions until this is dropped.
                    self.failed = true;
                    Poll::Ready(Err(Self::timed_out()))
                } else {
                    Poll::Pending
                }
            }
        }
    }

    /// Hand on what the NAMESPACE filter let through, as much as fits.
    fn pass(&mut self, buf: &mut ReadBuf<'_>) {
        let n = self.passing.len().min(buf.remaining());
        buf.put_slice(&self.passing[..n]);
        self.passing.drain(..n);
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for ImapIo<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if !this.passing.is_empty() {
            this.pass(buf);
            return Poll::Ready(Ok(()));
        }
        if this.namespace.is_none() {
            return this.poll_wire(cx, buf);
        }
        loop {
            let mut chunk = [0u8; 4096];
            let mut read = ReadBuf::new(&mut chunk);
            match this.poll_wire(cx, &mut read) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(())) => {}
            }
            if read.filled().is_empty() {
                // The end of the stream: what was held goes on before it.
                let filter = this.namespace.take().expect("held above");
                filter.finish(&mut this.passing);
                this.pass(buf);
                return Poll::Ready(Ok(()));
            }
            if let Some(filter) = this.namespace.as_mut() {
                filter.feed(read.filled(), &mut this.passing);
            }
            if !this.passing.is_empty() {
                this.pass(buf);
                return Poll::Ready(Ok(()));
            }
        }
    }
}

impl<S> ImapIo<S> {
    fn write_result<T>(
        &mut self,
        result: Poll<io::Result<T>>,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<T>> {
        match result {
            Poll::Ready(result) => {
                self.write_timer = None;
                Poll::Ready(result)
            }
            Poll::Pending if elapsed(&mut self.write_timer, self.timeout, cx) => {
                self.failed = true;
                Poll::Ready(Err(Self::timed_out()))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for ImapIo<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.failed {
            return Poll::Ready(Err(Self::timed_out()));
        }
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        self.write_result(result, cx)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.failed {
            return Poll::Ready(Err(Self::timed_out()));
        }
        let result = Pin::new(&mut self.inner).poll_flush(cx);
        self.write_result(result, cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.failed {
            return Poll::Ready(Err(Self::timed_out()));
        }
        let result = Pin::new(&mut self.inner).poll_shutdown(cx);
        self.write_result(result, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures::TryStreamExt;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream};
    use tokio::time::{sleep, Instant};

    async fn session() -> (
        async_imap::Session<ImapIo<DuplexStream>>,
        BufReader<DuplexStream>,
    ) {
        let (client, server) = tokio::io::duplex(1024);
        let login = tokio::spawn(async move {
            async_imap::Client::new(ImapIo::new(client))
                .login("test", "test")
                .await
                .unwrap()
        });
        let mut server = BufReader::new(server);
        let tag = command(&mut server, "LOGIN").await;
        server
            .get_mut()
            .write_all(format!("{tag} OK logged in\r\n").as_bytes())
            .await
            .unwrap();
        (login.await.unwrap(), server)
    }

    async fn command(server: &mut BufReader<DuplexStream>, expected: &str) -> String {
        let mut line = String::new();
        server.read_line(&mut line).await.unwrap();
        assert!(line.contains(expected), "{line:?}");
        line.split_whitespace().next().unwrap().to_string()
    }

    fn assert_timeout(error: async_imap::error::Error) {
        match error {
            async_imap::error::Error::Io(e) => assert_eq!(e.kind(), io::ErrorKind::TimedOut),
            e => panic!("expected I/O timeout, got {e}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn unanswered_search_times_out_with_the_connection_still_open() {
        let (mut client, mut server) = session().await;
        let search = tokio::spawn(async move { client.uid_search("ALL").await });
        command(&mut server, "UID SEARCH ALL").await;
        let started = Instant::now();
        assert_timeout(
            tokio::time::timeout(IO_TIMEOUT * 2, search)
                .await
                .expect("unanswered SEARCH did not release the worker")
                .unwrap()
                .unwrap_err(),
        );
        assert_eq!(started.elapsed(), IO_TIMEOUT);
        // The failed session is dropped, so the server sees EOF rather than
        // another request queued behind the unanswered search.
        assert_eq!(server.read(&mut [0]).await.unwrap(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn fetch_that_stalls_inside_a_literal_times_out_while_collecting() {
        let (mut client, mut server) = session().await;
        let fetch = tokio::spawn(async move {
            client
                .uid_fetch("7", "BODY.PEEK[]")
                .await
                .unwrap()
                .try_collect::<Vec<_>>()
                .await
        });
        command(&mut server, "UID FETCH").await;
        server
            .get_mut()
            .write_all(b"* 1 FETCH (UID 7 BODY[] {8}\r\nab")
            .await
            .unwrap();
        assert_timeout(
            tokio::time::timeout(IO_TIMEOUT * 2, fetch)
                .await
                .expect("incomplete FETCH literal did not release the worker")
                .unwrap()
                .unwrap_err(),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_long_download_keeps_going_while_bytes_arrive() {
        let (client, mut server) = tokio::io::duplex(16);
        let writer = tokio::spawn(async move {
            for byte in b"mail" {
                sleep(IO_TIMEOUT * 2 / 3).await;
                server.write_all(&[*byte]).await.unwrap();
            }
        });
        let started = Instant::now();
        let mut buf = [0; 4];
        ImapIo::new(client).read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"mail");
        assert!(started.elapsed() > IO_TIMEOUT * 2);
        writer.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn idle_can_stay_quiet_and_command_deadlines_resume_after_done() {
        let (client, mut server) = session().await;
        let idle = tokio::spawn(async move {
            let mut handle = client.idle();
            handle.init().await.unwrap();
            handle.as_mut().set_idle(true);
            let (wait, _stop) = handle.wait_with_timeout(IO_TIMEOUT * 2);
            assert_eq!(
                wait.await.unwrap(),
                async_imap::extensions::idle::IdleResponse::Timeout
            );
            handle.as_mut().set_idle(false);
            let mut client = handle.done().await.unwrap();
            client.uid_search("ALL").await
        });
        let tag = command(&mut server, "IDLE").await;
        server.get_mut().write_all(b"+ idling\r\n").await.unwrap();
        let started = Instant::now();
        command(&mut server, "DONE").await;
        assert_eq!(started.elapsed(), IO_TIMEOUT * 2);
        server
            .get_mut()
            .write_all(format!("{tag} OK idle ended\r\n").as_bytes())
            .await
            .unwrap();
        command(&mut server, "UID SEARCH ALL").await;
        assert_timeout(idle.await.unwrap().unwrap_err());
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_connection_rejects_late_replies_and_new_writes() {
        let (client, mut server) = tokio::io::duplex(16);
        let mut client = ImapIo::new(client);
        assert_eq!(
            client.read(&mut [0]).await.unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        server.write_all(b"late reply").await.unwrap();
        assert_eq!(
            client.read(&mut [0]).await.unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            client.write(b"new command").await.unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[tokio::test(start_paused = true)]
    async fn blocked_write_times_out() {
        let (client, _server) = tokio::io::duplex(1);
        let started = Instant::now();
        assert_eq!(
            ImapIo::new(client)
                .write_all(b"ab")
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(started.elapsed(), IO_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_a_read_does_not_restart_its_deadline() {
        let (client, _server) = tokio::io::duplex(16);
        let mut client = ImapIo::new(client);
        let started = Instant::now();
        assert!(tokio::time::timeout(IO_TIMEOUT / 2, client.read(&mut [0]))
            .await
            .is_err());
        assert_eq!(
            client.read(&mut [0]).await.unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(started.elapsed(), IO_TIMEOUT);
    }
    async fn reply(server: &mut BufReader<DuplexStream>, expected: &str, lines: &str) {
        let tag = command(server, expected).await;
        let answer = match lines {
            "NO" => format!("{tag} NO refused\r\n"),
            _ => format!("{lines}{tag} OK done\r\n"),
        };
        server.get_mut().write_all(answer.as_bytes()).await.unwrap();
    }

    /// The hierarchy as `read_hierarchy` hears it from a server that
    /// answers CAPABILITY with `caps` and the rest with `answers`, and
    /// what a SEARCH after it finds (the session must carry on).
    async fn hierarchy_from(
        caps: &str,
        answers: &[(&str, &str)],
    ) -> (Option<crate::models::FolderHierarchy>, usize) {
        let (mut client, mut server) = session().await;
        let task = tokio::spawn(async move {
            let h = super::super::namespace::read_hierarchy(&mut client, &[]).await;
            let found = client.uid_search("ALL").await.unwrap().len();
            (h, found)
        });
        reply(
            &mut server,
            "CAPABILITY",
            &format!("* CAPABILITY {caps}\r\n"),
        )
        .await;
        for (cmd, lines) in answers {
            reply(&mut server, cmd, lines).await;
        }
        reply(&mut server, "UID SEARCH", "* SEARCH 4 5\r\n").await;
        task.await.unwrap()
    }

    #[tokio::test]
    async fn namespace_is_read_and_the_session_carries_on() {
        use crate::models::FolderHierarchy;
        let courier = hierarchy_from(
            "IMAP4rev1 NAMESPACE",
            &[(
                "NAMESPACE",
                "* 2 EXISTS\r\n* NAMESPACE ((\"INBOX.\" \".\")) NIL NIL\r\n",
            )],
        )
        .await;
        assert_eq!(
            courier,
            (
                Some(FolderHierarchy::imap(Some('.'), Some("INBOX.".into()))),
                2
            )
        );
        // No personal namespace: the root's delimiter, nothing at the top.
        let shared = hierarchy_from(
            "IMAP4rev1 NAMESPACE",
            &[
                ("NAMESPACE", "* NAMESPACE NIL NIL ((\"\" \"/\"))\r\n"),
                ("LIST", "* LIST (\\Noselect) \"/\" \"\"\r\n"),
            ],
        )
        .await;
        assert_eq!(shared, (Some(FolderHierarchy::imap(Some('/'), None)), 2));
    }

    #[tokio::test]
    async fn without_namespace_list_gives_the_delimiter_or_none() {
        use crate::models::FolderHierarchy;
        let slash = hierarchy_from(
            "IMAP4rev1",
            &[
                ("LIST \"\" \"\"", "* LIST (\\Noselect) \"/\" \"\"\r\n"),
                ("LIST \"\" %", "* LIST () \"/\" \"INBOX\"\r\n"),
            ],
        )
        .await;
        assert_eq!(
            slash,
            (
                Some(FolderHierarchy::imap(Some('/'), Some(String::new())).unsaid()),
                2
            )
        );
        let flat =
            hierarchy_from("IMAP4rev1", &[("LIST", "* LIST (\\Noselect) NIL \"\"\r\n")]).await;
        assert_eq!(
            flat,
            (
                Some(FolderHierarchy::imap(None, Some(String::new())).unsaid()),
                2
            )
        );
        // NAMESPACE advertised but refused: LIST answers instead.
        let refused = hierarchy_from(
            "IMAP4rev1 NAMESPACE",
            &[
                ("NAMESPACE", "NO"),
                ("LIST", "* LIST () \"\\\\\" \"\"\r\n"),
                ("LIST", ""),
            ],
        )
        .await;
        assert_eq!(refused.0.and_then(|h| h.delimiter), Some('\\'));
        // Proton Bridge: no NAMESPACE, its folders kept in "Folders".
        let bridge = hierarchy_from(
            "IMAP4rev1 IDLE MOVE",
            &[
                ("LIST \"\" \"\"", "* LIST (\\Noselect) \"/\" \"\"\r\n"),
                (
                    "LIST \"\" %",
                    "* LIST (\\Noinferiors) \"/\" \"INBOX\"\r\n\
                     * LIST (\\Noselect \\Unmarked) \"/\" \"Labels\"\r\n\
                     * LIST (\\Noselect \\Unmarked) \"/\" \"Folders\"\r\n",
                ),
            ],
        )
        .await;
        assert_eq!(
            bridge,
            (
                Some(FolderHierarchy::imap(Some('/'), Some("Folders/".into()))),
                2
            )
        );
    }
}
