extern crate alloc;

mod auth;
mod error;
mod formats;
pub mod handshake;

use alloc::vec::Vec;
use bytes::Bytes;
use core::{convert::TryFrom, time::Duration};
use error::{Error, Result};
use snow::{params::NoiseParams, Builder, HandshakeState, TransportState};

pub use auth::{SignatureNoiseMessage, SignedPartHeader};
pub use formats::Certificate;

/// Static keypair (aka 's' and 'rs') from the noise handshake patterns. This has to be used by
/// users of this noise when Building the responder
pub use snow::Keypair as StaticKeypair;
/// Snow doesn't have a dedicated public key type, we will need it for authentication
pub type StaticPublicKey = Vec<u8>;
/// Snow doesn't have a dedicated secret key type, we will need it for authentication
pub type StaticSecretKey = Vec<u8>;

const PARAMS: &str = const_sv2::NOISE_PARAMS;

/// version: u16
/// valid_from: u32
/// not_valid_after: u32
/// siganture len: u16 (64 little endian)
/// siganture: 64 bytes
pub const SIGNATURE_MESSAGE_LEN: usize = 76;

/// Private snow constants redefined here
pub const MAX_MESSAGE_SIZE: usize = const_sv2::NOISE_FRAME_MAX_SIZE;
pub const SNOW_PSKLEN: usize = const_sv2::SNOW_PSKLEN;
pub const SNOW_TAGLEN: usize = const_sv2::SNOW_TAGLEN;
pub const HEADER_SIZE: usize = const_sv2::NOISE_FRAME_HEADER_SIZE;

const BUFFER_LEN: usize =
    SNOW_PSKLEN + SNOW_PSKLEN + SNOW_TAGLEN + SNOW_TAGLEN + SIGNATURE_MESSAGE_LEN;

/// Generates noise specific static keypair specific for the current params
pub fn generate_keypair() -> Result<StaticKeypair> {
    let params: NoiseParams = PARAMS.parse().expect("BUG: cannot parse noise parameters");
    let builder: Builder<'_> = Builder::new(params);
    builder.generate_keypair().map_err(|_| Error {})
}

/// Generate a random ed25519 dalek keypair
/// It return (public key, private key)
pub fn random_keypair() -> ([u8; 32], [u8; 32]) {
    let mut csprng = rand::rngs::OsRng {};
    let kp = ed25519_dalek::Keypair::generate(&mut csprng);
    (kp.public.to_bytes(), kp.secret.to_bytes())
}

#[derive(Debug)]
pub struct Initiator {
    stage: usize,
    handshake_state: HandshakeState,
    /// Authority public key use to sign the certificate that prove the identity of the Responder
    /// (upstream node) to the Initiator (downstream node)
    authority_public_key: ed25519_dalek::PublicKey,
}

impl Initiator {
    pub fn new(authority_public_key: ed25519_dalek::PublicKey) -> Result<Self> {
        let params: NoiseParams = PARAMS.parse().expect("BUG: cannot parse noise parameters");

        let builder: Builder<'_> = Builder::new(params);
        let handshake_state = builder.build_initiator().map_err(|_| Error {})?;

        Ok(Self {
            stage: 0,
            handshake_state,
            authority_public_key,
        })
    }

    pub fn from_raw_k(authority_public_key: [u8; 32]) -> Result<Self> {
        let authority_public_key = ed25519_dalek::PublicKey::from_bytes(&authority_public_key[..])
            .map_err(|_| Error {})?;
        Self::new(authority_public_key)
    }

    /// Verify the signature of the remote static key
    fn verify_remote_static_key_signature(
        &mut self,
        signature_noise_message: Vec<u8>,
    ) -> Result<()> {
        let remote_static_key = self.handshake_state.get_remote_static().ok_or(Error {})?;
        let remote_static_key = StaticPublicKey::from(remote_static_key);

        let signature_noise_message =
            auth::SignatureNoiseMessage::try_from(&signature_noise_message[..])
                .map_err(|_| Error {})?;

        let certificate = auth::Certificate::from_noise_message(
            signature_noise_message,
            remote_static_key,
            self.authority_public_key,
        );

        certificate.validate().map_err(|_| Error {})?;

        Ok(())
    }
}

impl handshake::Step for Initiator {
    fn into_handshake_state(self) -> HandshakeState {
        self.handshake_state
    }

    fn step(&mut self, in_msg: Option<handshake::Message>) -> Result<handshake::StepResult> {
        let mut noise_bytes = Vec::new();

        let result = match self.stage {
            0 => {
                // Create first message (initiator ephemeral public key)
                // -> e
                //
                let buffer_len = SNOW_PSKLEN + SNOW_TAGLEN;
                noise_bytes.resize(buffer_len, 0);

                let len_written = self
                    .handshake_state
                    .write_message(&[], &mut noise_bytes)
                    .map_err(|_| Error {})?;

                noise_bytes.truncate(len_written);

                handshake::StepResult::ExpectReply(noise_bytes)
            }
            1 => {
                // Receive responder message
                // <- e, ee, s, es, SIGNATURE_NOISE_MESSAGE
                //
                let in_msg = in_msg.ok_or(Error {})?;

                noise_bytes.resize(BUFFER_LEN, 0);

                let signature_len = self
                    .handshake_state
                    .read_message(&in_msg[..], &mut noise_bytes)
                    .map_err(|_| Error {})?;

                debug_assert!(SIGNATURE_MESSAGE_LEN == signature_len);

                self.verify_remote_static_key_signature(noise_bytes[..signature_len].to_vec())?;

                handshake::StepResult::Done
            }
            _ => {
                return Err(Error {});
            }
        };
        self.stage += 1;
        Ok(result)
    }
}

#[derive(Debug)]
pub struct Responder {
    stage: usize,
    handshake_state: HandshakeState,
    /// Serialized signature noise message
    signature_noise_message: Bytes,
}

pub struct Authority {
    kp: ed25519_dalek::Keypair,
}

impl Authority {
    pub fn new(kp: ed25519_dalek::Keypair) -> Self {
        Self { kp }
    }

    /// Create an Authority from pub_k and priv_k (32 bytes keys)
    pub fn from_raw_k(pub_k: &[u8], priv_k: &[u8]) -> Option<Self> {
        let kp = ed25519_dalek::Keypair::from_bytes(&[priv_k, pub_k].concat()).ok()?;
        Some(Self { kp })
    }

    /// Create a Certificate valid until now + duration for pub_k
    pub fn new_cert_from_raw(
        &self,
        pub_k: &[u8],
        duration: Duration,
    ) -> Result<auth::SignatureNoiseMessage> {
        let header = SignedPartHeader::with_duration(duration).map_err(|_| Error {})?;

        let signed_part = auth::SignedPart::new(header, pub_k.into(), self.kp.public);

        let signature = signed_part.sign_with(&self.kp).map_err(|_| Error {})?;

        let certificate = auth::Certificate::new(signed_part, signature);

        Ok(certificate.build_noise_message())
    }

    /// Create a Certificate valid until now + duration for pub_k
    pub fn new_cert(
        &self,
        pub_k: StaticPublicKey,
        duration: Duration,
    ) -> Result<auth::SignatureNoiseMessage> {
        self.new_cert_from_raw(&pub_k[..], duration)
    }
}

impl Responder {
    pub fn new(static_keypair: &StaticKeypair, signature_noise_message: Bytes) -> Result<Self> {
        let params: NoiseParams = PARAMS.parse().map_err(|_| Error {})?;

        let builder: Builder<'_> = Builder::new(params);

        let handshake_state = builder
            .local_private_key(&static_keypair.private)
            .build_responder()
            .expect("BUG: cannot build responder");

        Ok(Self {
            stage: 0,
            handshake_state,
            signature_noise_message,
        })
    }

    pub fn with_random_static_kp(signature_noise_message: Bytes) -> Result<Self> {
        let static_keypair = generate_keypair().map_err(|_| Error {})?;
        Self::new(&static_keypair, signature_noise_message)
    }

    /// Create a Responder from authority pub_k and priv_k (32 bytes keys)
    /// Usefull if there is no central pool authority and the Responder can certify itself
    pub fn from_authority_kp(
        pub_k: &[u8],
        priv_k: &[u8],
        duration: core::time::Duration,
    ) -> Result<Self> {
        let authority = Authority::from_raw_k(pub_k, priv_k);

        let static_keypair = generate_keypair().map_err(|_| Error {})?;

        let signature_noise_message = authority
            .ok_or(Error {})?
            .new_cert(static_keypair.public.clone(), duration)?
            .serialize_to_bytes_mut()?;

        Self::new(&static_keypair, signature_noise_message.into())
    }
}

impl handshake::Step for Responder {
    fn into_handshake_state(self) -> HandshakeState {
        self.handshake_state
    }

    fn step(&mut self, in_msg: Option<handshake::Message>) -> Result<handshake::StepResult> {
        let mut noise_bytes = Vec::new();

        let result = match self.stage {
            0 => {
                // Receive Initiator ephemeral public key
                // <- e
                //
                let in_msg = in_msg.ok_or(Error {})?;

                let buffer_len = BUFFER_LEN;

                noise_bytes.resize(buffer_len, 0);

                self.handshake_state
                    .read_message(&in_msg, &mut noise_bytes)
                    .map_err(|_| Error {})?;

                // Create response message
                // -> e, ee, s, es, SIGNATURE_NOISE_MESSAGE
                //
                let len_written = self
                    .handshake_state
                    .write_message(&self.signature_noise_message, &mut noise_bytes)
                    .map_err(|_| Error {})?;

                debug_assert!(buffer_len == len_written);
                handshake::StepResult::NoMoreReply(noise_bytes)
            }
            1 => handshake::StepResult::Done,
            _ => return Err(Error {}),
        };
        self.stage += 1;
        Ok(result)
    }
}

/// Helper struct that wraps the transport state and provides convenient interface to read/write
/// messages
#[derive(Debug)]
pub struct TransportMode {
    inner: TransportState,
}

impl TransportMode {
    pub fn new(inner: TransportState) -> Self {
        Self { inner }
    }

    /// Decrypt and verify message from `in_buf` and append the result to `decrypted_message`
    #[inline(always)]
    pub fn read(&mut self, encrypted_msg: &[u8], decrypted_msg: &mut [u8]) -> Result<()> {
        let _msg_len = self
            .inner
            .read_message(encrypted_msg, decrypted_msg)
            .map_err(|_| Error {})?;

        Ok(())
    }

    /// Return the size that decrypt_msg in Self::read should have in order to decrypt the
    /// encrypted payload
    ///
    ///
    #[inline(always)]
    pub fn size_hint_decrypt(encrypted_msg_len: usize) -> Option<usize> {
        encrypted_msg_len.checked_sub(SNOW_TAGLEN)
    }

    /// Return the size that encrypt_msg in Self::write should have in order to encrypt the payload
    ///
    #[inline(always)]
    pub fn size_hint_encrypt(payload_len: usize) -> usize {
        payload_len + SNOW_TAGLEN
    }

    /// Encrypt a message specified in `plain_msg` and write the encrypted message into a encrypted
    /// It also encode the length of the encrypted message as the first 2 bytes
    ///
    #[inline(always)]
    pub fn write(&mut self, plain_msg: &[u8], encrypted_msg: &mut [u8]) -> Result<()> {
        //let len = self.size_hint_encrypt(plain_msg) - HEADER_SIZE;
        //encrypted_msg[0] = len.to_le_bytes()[0];
        //encrypted_msg[1] = len.to_be_bytes()[1];

        let _msg_len = self
            .inner
            .write_message(plain_msg, encrypted_msg)
            .map_err(|_| Error {})?;

        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod test {
    use super::*;
    use bytes::BytesMut;
    use handshake::Step as _;

    /// Helper that builds:
    /// - serialized signature noise message
    /// - certification authority key pair
    /// - server (responder) static key pair
    fn build_serialized_signature_noise_message_and_keypairs(
    ) -> (Bytes, ed25519_dalek::Keypair, StaticKeypair) {
        let (signed_part, authority_keypair, static_keypair, signature) =
            auth::test::build_test_signed_part_and_auth();
        let certificate = auth::Certificate::new(signed_part, signature);
        let signature_noise_message = certificate
            .build_noise_message()
            .serialize_to_bytes_mut()
            .expect("BUG: Cannot serialize signature noise message")
            .freeze();
        (signature_noise_message, authority_keypair, static_keypair)
    }

    pub(crate) fn perform_handshake() -> (TransportMode, TransportMode) {
        // Prepare test certificate and a serialized noise message that contains the signature
        let (signature_noise_message, authority_keypair, static_keypair) =
            build_serialized_signature_noise_message_and_keypairs();

        let mut initiator = Initiator::new(authority_keypair.public).unwrap();

        let mut responder = Responder::new(&static_keypair, signature_noise_message).unwrap();
        let mut initiator_in_msg: Option<handshake::Message> = None;

        loop {
            match initiator
                .step(initiator_in_msg.clone())
                .expect("BUG: Initiator failed")
            {
                handshake::StepResult::ExpectReply(initiator_out_msg) => {
                    match responder
                        .step(Some(initiator_out_msg))
                        .expect("BUG: responder failed")
                    {
                        handshake::StepResult::ExpectReply(responder_out_msg) => {
                            (&mut initiator_in_msg).replace(responder_out_msg);
                        }
                        handshake::StepResult::NoMoreReply(responder_out_msg) => {
                            (&mut initiator_in_msg).replace(responder_out_msg);
                        }
                        handshake::StepResult::Done => (),
                    }
                }
                handshake::StepResult::NoMoreReply(initiator_out_msg) => {
                    match responder
                        .step(Some(initiator_out_msg))
                        .expect("BUG: responder failed")
                    {
                        handshake::StepResult::ExpectReply(responder_out_msg)
                        | handshake::StepResult::NoMoreReply(responder_out_msg) => panic!(
                            "BUG: Responder provided an unexpected response {:?}",
                            responder_out_msg
                        ),
                        handshake::StepResult::Done => (),
                    }
                }
                // Initiator is now finalized
                handshake::StepResult::Done => {
                    break;
                }
            };
        }

        // Above unwrapped:
        //let first_message = match initiator.step(None, BytesMut::new()).unwrap() {
        //        handshake::StepResult::ExpectReply(msg) => msg,
        //        _ => panic!(),
        //};
        //let second_message = match responder.step(Some(first_message), BytesMut::new()).unwrap() {
        //        handshake::StepResult::NoMoreReply(msg) => msg,
        //        _ => panic!(),
        //};
        //initiator.step(Some(second_message), BytesMut::new()).unwrap();

        let initiator_transport_mode = TransportMode::new(
            initiator
                .into_handshake_state()
                .into_transport_mode()
                .expect("BUG: cannot convert initiator into transport mode"),
        );
        let responder_transport_mode = TransportMode::new(
            responder
                .into_handshake_state()
                .into_transport_mode()
                .expect("BUG: cannot convert responder into transport mode"),
        );

        (initiator_transport_mode, responder_transport_mode)
    }

    /// Verifies that initiator and responder can successfully perform a handshake
    #[test]
    fn test_handshake() {
        perform_handshake();
    }

    #[test]
    fn test_handshake2() {
        let (signature_noise_message, authority_keypair, static_keypair) =
            build_serialized_signature_noise_message_and_keypairs();

        let mut initiator = Initiator::new(authority_keypair.public).unwrap();

        let mut responder = Responder::new(&static_keypair, signature_noise_message).unwrap();
        let first_message = match initiator.step(None).unwrap() {
            handshake::StepResult::ExpectReply(msg) => msg,
            _ => panic!(),
        };
        let second_message = match responder.step(Some(first_message)).unwrap() {
            handshake::StepResult::NoMoreReply(msg) => msg,
            _ => panic!(),
        };
        initiator.step(Some(second_message)).unwrap();

        TransportMode::new(
            initiator
                .into_handshake_state()
                .into_transport_mode()
                .unwrap(),
        );
        TransportMode::new(
            responder
                .into_handshake_state()
                .into_transport_mode()
                .unwrap(),
        );
    }

    /// Verifies that initiator and responder can successfully send/receive message after
    /// handshake;
    #[test]
    fn test_send_message() {
        let (mut initiator_transport_mode, mut responder_transport_mode) = perform_handshake();

        let message = b"test message";
        let mut encrypted_msg = BytesMut::new();
        let mut decrypted_msg = BytesMut::new();

        let size_hint = TransportMode::size_hint_encrypt(message.len());
        encrypted_msg.resize(size_hint, 0);

        initiator_transport_mode
            .write(&message[..], &mut encrypted_msg)
            .unwrap();

        let size_hint = TransportMode::size_hint_decrypt(encrypted_msg.len());
        decrypted_msg.resize(size_hint.unwrap(), 0);

        responder_transport_mode
            .read(&encrypted_msg[..], &mut decrypted_msg[..])
            .unwrap();

        assert_eq!(&message[..], &decrypted_msg[..], "Messages don't match");
    }
}
