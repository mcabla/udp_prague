//! Network/socket backend used by the runtime.

pub mod udpsocket;

pub use self::udpsocket::{Endpoint, SocketPlatformSupport, UDPSocket};
