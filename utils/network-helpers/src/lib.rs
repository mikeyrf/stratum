#[cfg(feature = "async_std")]
mod noise_connection_async_std;
#[cfg(feature = "async_std")]
mod plain_connection_async_std;
#[cfg(feature = "async_std")]
pub use noise_connection_async_std::{connect, listen, Connection};
#[cfg(feature = "async_std")]
pub use plain_connection_async_std::{plain_connect, plain_listen, PlainConnection};
