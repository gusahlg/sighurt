//! A calloop source that reads a non-blocking terminal fd into the parser.
//!
//! The fd must already be non-blocking and in raw mode. A cooked terminal
//! does not report input as readable until newline, so a partial escape
//! sequence never reaches the parser. The source cannot shorten the loop's
//! sleep by itself. Pass [`InputSource::poll_timeout`] as the `dispatch`
//! timeout, then call [`InputSource::expire_if_due`] so a lone Escape is not
//! held forever and a partial reply is not cut on the Escape timer.

use std::io;
use std::os::fd::AsFd;

use calloop::generic::Generic;
use calloop::{EventSource, Interest, Mode, Poll, PostAction, Readiness, Token, TokenFactory};
use rustix::io::Errno;

use crate::parser::{Input, Parser};

/// Parser plus the fd it reads.
pub struct InputSource<F: AsFd> {
    inner: Generic<F>,
    parser: Parser,
}

impl<F: AsFd> InputSource<F> {
    /// `fd` should already be non-blocking. Interest is read, level-triggered.
    pub fn new(fd: F) -> Self {
        Self {
            inner: Generic::new(fd, Interest::READ, Mode::Level),
            parser: Parser::new(),
        }
    }

    /// Time until a partial sequence should be expired.
    pub fn poll_timeout(&self) -> Option<std::time::Duration> {
        self.parser.poll_timeout()
    }

    /// The partial-sequence timer fired.
    pub fn expire(&mut self) -> Vec<Input> {
        self.parser.expire()
    }

    /// Expire only if [`Self::poll_timeout`] has elapsed.
    pub fn expire_if_due(&mut self) -> Vec<Input> {
        self.parser.expire_if_due()
    }
}

impl<F: AsFd> EventSource for InputSource<F> {
    type Event = Vec<Input>;
    type Metadata = ();
    type Ret = io::Result<PostAction>;
    type Error = io::Error;

    fn process_events<C>(
        &mut self,
        readiness: Readiness,
        token: Token,
        mut callback: C,
    ) -> Result<PostAction, Self::Error>
    where
        C: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        let parser = &mut self.parser;
        let mut action = PostAction::Continue;
        self.inner
            .process_events(readiness, token, |readiness, file| {
                if readiness.readable || readiness.error {
                    let mut buf = [0u8; 8192];
                    loop {
                        match rustix::io::read(file.as_fd(), &mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                let events = parser.feed(&buf[..n]);
                                if !events.is_empty() {
                                    let mut metadata = ();
                                    action = callback(events, &mut metadata)?;
                                }
                            }
                            Err(Errno::INTR) => continue,
                            Err(Errno::AGAIN) => break,
                            Err(err) => return Err(io::Error::from(err)),
                        }
                    }
                }
                Ok(PostAction::Continue)
            })?;
        Ok(action)
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        self.inner.register(poll, token_factory)
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        self.inner.reregister(poll, token_factory)
    }

    fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
        self.inner.unregister(poll)
    }
}
