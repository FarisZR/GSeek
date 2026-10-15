//! HTTP framing stays in ureq; this transport can connect only to our private socket.
use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};
use ureq::unversioned::{
    resolver::DefaultResolver,
    transport::{Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, Transport},
};

pub fn agent(socket: PathBuf, timeout: Duration) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .max_idle_connections(0)
        .timeout_global(Some(timeout))
        .build();
    ureq::Agent::with_parts(config, UnixConnector(socket), DefaultResolver::default())
}
#[derive(Debug)]
struct UnixConnector(PathBuf);
impl Connector for UnixConnector {
    type Out = UnixTransport;
    fn connect(
        &self,
        details: &ConnectionDetails,
        _: Option<()>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(Some(UnixTransport {
            stream: UnixStream::connect(&self.0)?,
            buffers: LazyBuffers::new(
                details.config.input_buffer_size(),
                details.config.output_buffer_size(),
            ),
        }))
    }
}
#[derive(Debug)]
struct UnixTransport {
    stream: UnixStream,
    buffers: LazyBuffers,
}
fn io_error(error: io::Error, timeout: NextTimeout) -> ureq::Error {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => ureq::Error::Timeout(timeout.reason),
        _ => error.into(),
    }
}
impl Transport for UnixTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }
    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.stream
            .set_write_timeout(timeout.not_zero().map(|v| *v))?;
        self.stream
            .write_all(&self.buffers.output()[..amount])
            .map_err(|e| io_error(e, timeout))
    }
    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        self.stream
            .set_read_timeout(timeout.not_zero().map(|v| *v))?;
        let amount = self
            .stream
            .read(self.buffers.input_append_buf())
            .map_err(|e| io_error(e, timeout))?;
        self.buffers.input_appended(amount);
        Ok(amount > 0)
    }
    // Each embedding uses a fresh connection; no pooled stale sockets.
    fn is_open(&mut self) -> bool {
        false
    }
}
