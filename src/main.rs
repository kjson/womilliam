extern crate tokio;
#[macro_use]
extern crate futures;
extern crate bytes;

use tokio::io;
use tokio::net::{TcpListener, TcpStream};
use tokio::prelude::*;
use futures::sync::mpsc;
use futures::future::{self, Either};
use bytes::{BytesMut, Bytes, BufMut};

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};


/// Shorthand for the transmit half of the message channel.
type Tx = mpsc::UnboundedSender<Bytes>;

/// Shorthand for the receive half of the message channel.
type Rx = mpsc::UnboundedReceiver<Bytes>;


struct Shared {
    peers: HashMap<SocketAddr, Tx>,
}


impl Shared {
    // TODO do I have to implement this.
    fn new() -> Self {
        Shared { peers: HashMap::new() }
    }
}


struct Lines {
    socket: TcpStream,
    rd: BytesMut,
    wr: BytesMut,
}


impl Lines {
    fn new(socket: TcpStream) -> Self {
        Lines {
            socket,
            rd: BytesMut::new(),
            wr: BytesMut::new(),
        }
    }

    // Reads from socket until there's no more bytes to read.
    fn fill_read_buf(&mut self) -> Result<Async<()>, io::Error> {
        loop {
            self.rd.reserve(1024);
            if try_ready!(self.socket.read_buf(&mut self.rd)) == 0 {
                return Ok(Async::Ready(()));
            }
        }
    }

    fn buffer(&mut self, line: &[u8]) {
        self.wr.put(line);
    }

    fn poll_flush(&mut self) -> Poll<(), io::Error> {
        while !self.wr.is_empty() {
            let n = try_ready!(self.socket.poll_write(&self.wr));
            assert!(n > 0);
            let _ = self.wr.split_to(n);
        }

        Ok(Async::Ready(()))
    }
}


impl Stream for Lines {
    type Item = BytesMut;
    type Error = io::Error;

    fn poll(&mut self) -> Result<Async<Option<Self::Item>>, Self::Error> {

        let sock_closed = self.fill_read_buf()?.is_ready();

        // Finds the position of the first occurrence of \r\n
        let pos = self.rd.windows(2).position(|bytes| bytes == b"\r\n");

        // Trims rd up until the first \r\n
        if let Some(pos) = pos {
            let mut line = self.rd.split_to(pos + 2);
            line.split_off(pos);
            return Ok(Async::Ready(Some(line)));
        }

        if sock_closed {
            Ok(Async::Ready(None))
        } else {
            Ok(Async::NotReady)
        }
    }
}


struct Peer {
    /// Name of the peer.
    name: BytesMut,

    /// A TCP socket wrapped by Lines as a stream.
    lines: Lines,

    /// Shared access to the chat hashmap.
    state: Arc<Mutex<Shared>>,

    /// Receive half of the message channel.
    rx: Rx,

    /// The peers address.
    addr: SocketAddr,
}


impl Peer {
    fn new(name: BytesMut, state: Arc<Mutex<Shared>>, lines: Lines) -> Self {
        let addr = lines.socket.peer_addr().unwrap();
        let (tx, rx) = mpsc::unbounded();
        state.lock().unwrap().peers.insert(addr, tx);
        Peer {
            name,
            lines,
            state,
            rx,
            addr,
        }
    }
}


/// When the peer disconnects we need to remove it from the global hashmap of peers.
impl Drop for Peer {
    fn drop(&mut self) {
        self.state.lock().unwrap().peers.remove(&self.addr);
    }
}


impl Future for Peer {
    type Item = ();
    type Error = io::Error;

    fn poll(&mut self) -> Poll<(), io::Error> {

        while let Async::Ready(Some(v)) = self.rx.poll().unwrap() {
            // Move all the data into this peers write buffer.
            self.lines.buffer(&v);
        }

        // Flush all the data to the socket.
        let _ = self.lines.poll_flush()?;


        // Read from the peers socket.
        while let Async::Ready(line) = self.lines.poll()? {

            if let Some(message) = line {
                let mut line = self.name.clone();
                line.put(": ");
                line.put(&message);
                println!("{:#?}", line.clone());

                line.put("\r\n");

                // We must make this line immutable so that it can be cloned without copying.
                let line = line.freeze();

                // Broadcast to all other peers.
                for (addr, tx) in &self.state.lock().unwrap().peers {
                    // Don't send the message to ourself.
                    if *addr != self.addr {
                        tx.unbounded_send(line.clone()).unwrap();
                    }
                }


            } else {
                return Ok(Async::Ready(()));
            }
        }

        Ok(Async::NotReady)
    }
}


fn process(socket: TcpStream, state: Arc<Mutex<Shared>>) {
    let lines = Lines::new(socket);

    let connection = lines
        .into_future()
        .map_err(|(e, _)| e)
        .and_then(|(name, lines)| {
            let name = match name {
                Some(name) => name,
                None => {
                    return Either::A(future::ok(()));
                }
            };

            println!("`{:?}` is joining the chat", name);


            let peer = Peer::new(name, state, lines);
            Either::B(peer)
        })
        .map_err(|e| {
            println!("connection error = {:?}", e);
        });

    tokio::spawn(connection);
}

fn main() {
    // Shared map of peers to transmission channel
    let state = Arc::new(Mutex::new(Shared::new()));

    // TODO command line args.
    let addr = "127.0.0.1:6142".parse().unwrap();
    let listener = TcpListener::bind(&addr).unwrap();

    let server = listener
        .incoming()
        .for_each(move |socket| {
            process(socket, state.clone());
            Ok(())
        })
        .map_err(|err| {
            println!("Connection error: {:?}", err);
        });

    println!("server running on localhost:6142");

    tokio::run(server);

}
