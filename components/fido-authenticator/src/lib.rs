 #![cfg_attr(not(test), no_std)]

use core::task::Poll;
use core::convert::{TryFrom, TryInto};
use core::iter::FromIterator;

use cortex_m_semihosting::hprintln;

use crypto_service::{
    Client as CryptoClient,
    pipe::Syscall as CryptoSyscall,
    types::{
        KeySerialization,
        Mechanism,
        MediumData,
        Message,
        ObjectHandle,
        SignatureSerialization,
        StorageLocation,
        StorageAttributes,
    },
};
use ctap_types::{
    Bytes, consts, String, Vec,
    cose::P256PublicKey as CoseP256PublicKey,
    cose::PublicKey as CosePublicKey,
    rpc::AuthenticatorEndpoint,
    authenticator::{ctap1, ctap2, Error, Request, Response},
};

pub mod credential;
pub use credential::*;

type Result<T> = core::result::Result<T, Error>;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(i32)]
enum SupportedAlgorithm {
    P256 = -7,
    Ed25519 = -8,
}

/// Idea is to maybe send a request over a queue,
/// and return upon button press.
/// TODO: Do we need a timeout?
pub trait UserPresence {
    fn user_present(&mut self) -> bool;
}

pub struct SilentAuthenticator {}

impl UserPresence for SilentAuthenticator {
    fn user_present(&mut self) -> bool {
        true
    }
}

fn cbor_serialize_message<T: serde::Serialize>(object: &T) -> core::result::Result<Message, ctap_types::serde::Error> {
    let mut message = Message::new();
    ctap_types::serde::cbor_serialize_bytes(object, &mut message)?;
    Ok(message)
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct Configuration {
    aaguid: Bytes<consts::U16>,
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct State {
    attestation_key: Option<ObjectHandle>,
    counter: Option<ObjectHandle>,
    key_agreement_key: Option<ObjectHandle>,
    key_encryption_key: Option<ObjectHandle>,
    key_wrapping_key: Option<ObjectHandle>,
    pin_token: Option<ObjectHandle>,
    retries: Option<u8>,
    consecutive_pin_mismatches: u8,
    pin_hash: Option<[u8; 16]>,
}

// impl State {
//     pub fn key_agreement_key(crypto: &mut CryptoClient
// }

pub struct Authenticator<'a, S, UP>
where
    S: CryptoSyscall,
    UP: UserPresence,
{
    config: Configuration,
    crypto: CryptoClient<'a, S>,
    rpc: AuthenticatorEndpoint<'a>,
    state: State,
    up: UP,
}

// #[derive(Clone, Debug)]
// pub enum Error {
//     Catchall,
// }

macro_rules! block {
    ($future_result:expr) => {{
        // evaluate the expression
        let mut future_result = $future_result;
        loop {
            match future_result.poll() {
                Poll::Ready(result) => { break result; },
                Poll::Pending => {},
            }
        }
    }}
}

macro_rules! syscall {
    ($pre_future_result:expr) => {{
        // evaluate the expression
        let mut future_result = $pre_future_result.expect("no client error");
        loop {
            match future_result.poll() {
                Poll::Ready(result) => { break result.expect("no errors"); },
                Poll::Pending => {},
            }
        }
    }}
}

impl<'a, S: CryptoSyscall, UP: UserPresence> Authenticator<'a, S, UP> {

    pub fn new(crypto: CryptoClient<'a, S>, rpc: AuthenticatorEndpoint<'a>, up: UP) -> Self {

        let config = Configuration {
            aaguid: Bytes::try_from_slice(b"AAGUID0123456789").unwrap(),
        };
        let state = State::default();
        let authenticator = Authenticator { config, crypto, rpc, state, up };

        authenticator
    }

    pub fn attestation_key(&mut self) -> Result<ObjectHandle> {
        match self.state.attestation_key.clone() {
            Some(key) => Ok(key),
            None => self.rotate_attestation_key(),
        }
    }

    // TODO: How to inject this?
    pub fn rotate_attestation_key(&mut self) -> Result<ObjectHandle> {
        // TODO: delete old one first
        let key = block!(self.crypto
            .generate_p256_private_key(StorageLocation::Internal).map_err(|_| Error::Other)?)
            .map_err(|_| Error::Other)?.key;
        self.state.attestation_key = Some(key.clone());
        Ok(key)
    }

    pub fn key_agreement_key(&mut self) -> Result<ObjectHandle> {
        match self.state.key_agreement_key.clone() {
            Some(key) => Ok(key),
            None => self.rotate_key_agreement_key(),
        }
    }

    pub fn rotate_key_encryption_key(&mut self) -> Result<ObjectHandle> {
        // TODO: delete old one first
        let key = block!(self.crypto
            .generate_chacha8poly1305_key(StorageLocation::Volatile).map_err(|_| Error::Other)?)
            .map_err(|_| Error::Other)?.key;
        self.state.key_encryption_key = Some(key.clone());
        Ok(key)
    }

    pub fn key_encryption_key(&mut self) -> Result<ObjectHandle> {
        match self.state.key_encryption_key.clone() {
            Some(key) => Ok(key),
            None => self.rotate_key_encryption_key(),
        }
    }

    pub fn rotate_key_agreement_key(&mut self) -> Result<ObjectHandle> {
        let key = block!(self.crypto
            .generate_p256_private_key(StorageLocation::Volatile).map_err(|_| Error::Other)?)
            .map_err(|_| Error::Other)?.key;
        self.state.key_agreement_key = Some(key.clone());
        Ok(key)
    }

    pub fn consecutive_pin_mismatches(&mut self) -> u8 {
        self.state.consecutive_pin_mismatches
    }

    pub fn retries(&mut self) -> Result<u8> {
        match self.state.retries {
            Some(retries) => Ok(retries),
            None => {
                self.state.retries = Some(8);
                Ok(8)
            }
        }
    }

    pub fn reset_retries(&mut self) -> Result<()> {
        self.state.retries = Some(8);
        self.state.consecutive_pin_mismatches = 0;
        Ok(())
    }

    pub fn decrement_retries(&mut self) -> Result<()> {
        // error to call before initialization
        self.state.retries = Some(self.state.retries.unwrap() - 1);
        self.state.consecutive_pin_mismatches += 1;
        Ok(())
    }

    pub fn pin_token(&mut self) -> Result<ObjectHandle> {
        match self.state.pin_token.clone() {
            Some(key) => Ok(key),
            None => self.rotate_pin_token(),
        }
    }

    pub fn rotate_pin_token(&mut self) -> Result<ObjectHandle> {
        let key = syscall!(self.crypto.generate_hmacsha256_key(StorageLocation::Volatile)).key;
        self.state.pin_token = Some(key.clone());
        Ok(key)
    }

    pub fn pin_is_set(&self) -> bool {
        self.state.pin_hash.is_some()
    }

    // pub(crate) fn config(&mut self) -> Result<C
    //     Err(Error::Initialisation)
    // }

    fn respond(&mut self, response: Result<Response>) {
        self.rpc.send.enqueue(response).expect("internal error");
    }

    pub fn poll(&mut self) {
        let _kek = self.key_agreement_key().unwrap();
        // hprintln!("polling authnr, kek = {:?}", &kek).ok();

        match self.rpc.recv.dequeue() {
            None => {},
            Some(request) => {
                // hprintln!("request: {:?}", &request).ok();

                use ctap_types::authenticator::{Error, Request, Response};

                match request {
                    Request::Ctap2(request) => {
                        match request {

                            // 0x4
                            ctap2::Request::GetInfo => {
                                let response = self.get_info();
                                self.rpc.send.enqueue(
                                    Ok(Response::Ctap2(ctap2::Response::GetInfo(response))))
                                    .expect("internal error");
                            }

                            // 0x1
                            ctap2::Request::GetAssertion(parameters) => {
                                // hprintln!("GA: {:?}", &parameters).ok();
                                let response = self.get_assertion(&parameters);
                                self.rpc.send.enqueue(
                                    match response {
                                        Ok(response) => Ok(Response::Ctap2(ctap2::Response::GetAssertion(response))),
                                        Err(error) => Err(error)
                                    })
                                    .expect("internal error");
                                hprintln!("enqueued GA response").ok();
                            }

                            // 0x2
                            ctap2::Request::MakeCredential(parameters) => {
                                // hprintln!("MC: {:?}", &parameters).ok();
                                let response = self.make_credential(&parameters);
                                self.rpc.send.enqueue(
                                    match response {
                                        Ok(response) => Ok(Response::Ctap2(ctap2::Response::MakeCredential(response))),
                                        Err(error) => Err(error)
                                    })
                                    .expect("internal error");
                                hprintln!("enqueued MC response").ok();
                            }

                            // 0x6
                            ctap2::Request::ClientPin(parameters) => {
                                let response = self.client_pin(&parameters);
                                self.rpc.send.enqueue(
                                    match response {
                                        Ok(response) => Ok(Response::Ctap2(ctap2::Response::ClientPin(response))),
                                        Err(error) => Err(error)
                                    })
                                    .expect("internal error");
                                hprintln!("enqueued CP response").ok();
                            }
                            _ => {
                                hprintln!("not implemented: {:?}", &request).ok();
                                self.rpc.send.enqueue(Err(Error::InvalidCommand)).expect("internal error");
                            }
                        }
                    }
                    Request::Ctap1(request) => {
                        hprintln!("ctap1 not implemented: {:?}", &request).ok();
                        // self.rpc.send.enqueue(Err(Error::InvalidCommand)).expect("internal error");
                        self.respond(Err(Error::InvalidCommand));
                    }
                }
            }
        }
    }

    fn client_pin(&mut self, parameters: &ctap2::client_pin::Parameters) -> Result<ctap2::client_pin::Response> {
        use ctap2::client_pin::PinV1Subcommand as Subcommand;
        hprintln!("processing CP").ok();

        if parameters.pin_protocol != 1{
            return Err(Error::InvalidParameter);
        }

        Ok(match parameters.sub_command {

            Subcommand::GetRetries => {
                hprintln!("processing CP.GR").ok();

                ctap2::client_pin::Response {
                    key_agreement: None,
                    pin_token: None,
                    retries: Some(self.retries().unwrap()),
                }
            }

            Subcommand::GetKeyAgreement => {
                hprintln!("processing CP.GKA").ok();

                let private_key = self.key_agreement_key().unwrap();
                let public_key = syscall!(self.crypto.derive_p256_public_key(&private_key, StorageLocation::Volatile)).key;
                let serialized_cose_key = syscall!(self.crypto.serialize_key(
                    Mechanism::P256, public_key, KeySerialization::Cose)).serialized_key;
                let cose_key = crypto_service::cbor_deserialize(&serialized_cose_key).unwrap();

                // TODO: delete public key

                ctap2::client_pin::Response {
                    key_agreement: cose_key,
                    pin_token: None,
                    retries: None,
                }
            }

            Subcommand::SetPin => {
                // 1. check mandatory parameters
                let platform_kek = match parameters.key_agreement.as_ref() {
                    Some(key) => key,
                    None => { return Err(Error::MissingParameter); }
                };
                let new_pin_enc = match parameters.new_pin_enc.as_ref() {
                    Some(pin) => pin,
                    None => { return Err(Error::MissingParameter); }
                };
                let pin_auth = match parameters.pin_auth.as_ref() {
                    Some(auth) => auth,
                    None => { return Err(Error::MissingParameter); }
                };

                // 2. is pin already set
                if self.pin_is_set() {
                    return Err(Error::PinAuthInvalid);
                }

                // 3. generate shared secret
                let shared_secret = self.generate_shared_secret(platform_kek)?;

                // 4. verify pinAuth
                self.verify_pin_auth(&shared_secret, new_pin_enc, pin_auth)?;

                // 5. decrypt and verify new PIN
                let new_pin = self.decrypt_pin_check_length(&shared_secret, new_pin_enc)?;

                // 6. store LEFT(SHA-256(newPin), 16), set retries to 8
                self.hash_store_pin(&new_pin)?;
                self.reset_retries();

                ctap2::client_pin::Response {
                    key_agreement: None,
                    pin_token: None,
                    retries: None,
                }
            }

            Subcommand::ChangePin => {

                // 1. check mandatory parameters
                let platform_kek = match parameters.key_agreement.as_ref() {
                    Some(key) => key,
                    None => { return Err(Error::MissingParameter); }
                };
                let pin_hash_enc = match parameters.pin_hash_enc.as_ref() {
                    Some(hash) => hash,
                    None => { return Err(Error::MissingParameter); }
                };
                let new_pin_enc = match parameters.new_pin_enc.as_ref() {
                    Some(pin) => pin,
                    None => { return Err(Error::MissingParameter); }
                };
                let pin_auth = match parameters.pin_auth.as_ref() {
                    Some(auth) => auth,
                    None => { return Err(Error::MissingParameter); }
                };

                // 2. fail if no retries left
                if self.retries().unwrap() == 0 {
                    return Err(Error::PinBlocked);
                }

                // 3. generate shared secret
                let shared_secret = self.generate_shared_secret(platform_kek)?;

                // 4. verify pinAuth
                let mut data = MediumData::new();
                data.extend_from_slice(new_pin_enc).map_err(|_| Error::InvalidParameter)?;
                data.extend_from_slice(pin_hash_enc).map_err(|_| Error::InvalidParameter)?;
                self.verify_pin_auth(&shared_secret, &data, pin_auth)?;

                // 5. decrement retries
                self.decrement_retries().unwrap();

                // 6. decrypt pinHashEnc, compare with stored
                self.decrypt_pin_hash_and_maybe_escalate(&shared_secret, &pin_hash_enc)?;

                // 7. reset retries
                self.reset_retries()?;

                // 8. decrypt and verify new PIN
                let new_pin = self.decrypt_pin_check_length(&shared_secret, new_pin_enc)?;

                // 9. store hashed PIN
                self.hash_store_pin(&new_pin)?;

                ctap2::client_pin::Response {
                    key_agreement: None,
                    pin_token: None,
                    retries: None,
                }
            }

            Subcommand::GetPinToken => {
                hprintln!("processing CP.GKA").ok();

                // 1. check mandatory parameters
                let platform_kek = match parameters.key_agreement.as_ref() {
                    Some(key) => key,
                    None => { return Err(Error::MissingParameter); }
                };
                let pin_hash_enc = match parameters.pin_hash_enc.as_ref() {
                    Some(hash) => hash,
                    None => { return Err(Error::MissingParameter); }
                };

                // 2. fail if no retries left
                if self.retries().unwrap() == 0 {
                    return Err(Error::PinBlocked);
                }

                // 3. generate shared secret
                let shared_secret = self.generate_shared_secret(platform_kek)?;

                // 4. decrement retires
                self.decrement_retries().unwrap();

                // 5. decrypt and verify pinHashEnc
                self.decrypt_pin_hash_and_maybe_escalate(&shared_secret, &pin_hash_enc)?;

                // 6. reset retries
                self.reset_retries()?;

                // 7. return encrypted pinToken
                let pin_token = self.pin_token().unwrap();
                let pin_token_enc = syscall!(self.crypto.wrap_key_aes256cbc(&shared_secret, &pin_token)).wrapped_key;

                // ble...
                if pin_token_enc.len() != 32 {
                    return Err(Error::Other);
                }
                let pin_token_enc_32 = Bytes::<consts::U32>::try_from_slice(&pin_token_enc).unwrap();

                ctap2::client_pin::Response {
                    key_agreement: None,
                    pin_token: Some(pin_token_enc_32),
                    retries: None,
                }
            }

        })
    }

    fn decrypt_pin_hash_and_maybe_escalate(&mut self, shared_secret: &ObjectHandle, pin_hash_enc: &Bytes<consts::U64>)
        -> Result<()>
    {
        let pin_hash = syscall!(self.crypto.decrypt_aes256cbc(
            &shared_secret, pin_hash_enc)).plaintext.ok_or(Error::Other)?;

        let stored_pin_hash = match self.state.pin_hash {
            Some(hash) => hash,
            None => { return Err(Error::InvalidCommand); }
        };

        if &pin_hash != &stored_pin_hash {
            // I) generate new KEK
            self.rotate_key_agreement_key()?;
            if self.retries().unwrap() == 0 {
                return Err(Error::PinBlocked);
            }
            if self.consecutive_pin_mismatches() >= 3 {
                return Err(Error::PinAuthBlocked);
            }
            return Err(Error::PinInvalid);
        }

        Ok(())
    }

    fn hash_store_pin(&mut self, pin: &Message) -> Result<()> {
        let pin_hash_32 = syscall!(self.crypto.hash_sha256(&pin)).hash;
        let pin_hash: [u8; 16] = pin_hash_32[..16].try_into().unwrap();
        self.state.pin_hash = Some(pin_hash);

        Ok(())
    }

    fn decrypt_pin_check_length(&mut self, shared_secret: &ObjectHandle, pin_enc: &[u8]) -> Result<Message> {
        let mut pin = syscall!(self.crypto.decrypt_aes256cbc(
            &shared_secret, &pin_enc)).plaintext.ok_or(Error::Other)?;

        // it is expected to be filled with null bytes to length at least 64
        if pin.len() < 64 {
            // correct error?
            return Err(Error::PinPolicyViolation);
        }

        // chop off null bytes
        let pin_length = pin.iter().position(|&b| b == b'\0').unwrap_or(pin.len());
        if pin_length < 4 {
            return Err(Error::PinPolicyViolation);
        }

        pin.resize_default(pin_length).unwrap();

        Ok(pin)
    }


    // fn verify_pin(&mut self, pin_auth: &Bytes<consts::U16>, client_data_hash: &Bytes<consts::U32>) -> bool {
    fn verify_pin(&mut self, pin_auth: &[u8; 16], data: &[u8]) -> Result<()> {
        let key = self.pin_token().unwrap();
        let tag = syscall!(self.crypto.sign_hmacsha256(&key, data)).signature;
        if pin_auth == &tag[..16] {
            Ok(())
        } else {
            Err(Error::PinAuthInvalid)
        }
    }

    fn verify_pin_auth(&mut self, shared_secret: &ObjectHandle, data: &[u8], pin_auth: &Bytes<consts::U16>)
        -> Result<()>
    {
        let expected_pin_auth = syscall!(self.crypto.sign_hmacsha256(shared_secret, data)).signature;

        if &expected_pin_auth[..16] == &pin_auth[..] {
            Ok(())
        } else {
            Err(Error::PinAuthInvalid)
        }
    }

    fn generate_shared_secret(&mut self, platform_key_agreement_key: &CoseP256PublicKey) -> Result<ObjectHandle> {
        let private_key = self.key_agreement_key().unwrap();
        let _public_key = syscall!(self.crypto.derive_p256_public_key(&private_key, StorageLocation::Volatile)).key;

        // let platform_kek = match &platform_key_agreement_key {
        //     Some(kek) => kek,
        //     None => {
        //         return Err(Error::MissingParameter);
        //     }
        // };
        let serialized_kek = cbor_serialize_message(platform_key_agreement_key).map_err(|_| Error::InvalidParameter)?;
        let platform_kek = syscall!(
            self.crypto.deserialize_key(
                Mechanism::P256, serialized_kek, KeySerialization::Cose,
                StorageAttributes::new().set_persistence(StorageLocation::Volatile))
            .map_err(|_| Error::InvalidParameter)).key;

        let pre_shared_secret = syscall!(self.crypto.agree(
            Mechanism::P256, private_key.clone(), platform_kek,
            StorageAttributes::new().set_persistence(StorageLocation::Volatile),
        )).shared_secret;

        let shared_secret = syscall!(self.crypto.derive_key(
            Mechanism::Sha256, pre_shared_secret, StorageAttributes::new().set_persistence(StorageLocation::Volatile)
        )).key;

        Ok(shared_secret)
    }


    /// Returns whether UV was performed.
    fn pin_prechecks(&mut self,
        options: &Option<ctap2::AuthenticatorOptions>,
        pin_auth: &Option<ctap2::PinAuth>,
        pin_protocol: &Option<u32>,
        data: &[u8],
    )
        -> Result<bool>
    {
        // 1. pinAuth zero length -> wait for user touch, then
        // return PinNotSet if not set, PinInvalid if set
        //
        // the idea is for multi-authnr scenario where platform
        // wants to enforce PIN and needs to figure out which authnrs support PIN
        if let Some(pin_auth) = pin_auth.as_ref() {
            if pin_auth.len() == 0 {
                if !self.up.user_present() {
                    return Err(Error::OperationDenied);
                }
                if !self.pin_is_set() {
                    return Err(Error::PinNotSet);
                } else {
                    return Err(Error::PinAuthInvalid);
                }
            }
        }

        // 2. check PIN protocol is 1 if pinAuth was sent
        if let Some(ref _pin_auth) = pin_auth {
            if let Some(1) = pin_protocol {
            } else {
                return Err(Error::PinAuthInvalid);
            }
        }

        // 3. if no PIN is set (we have no other form of UV),
        // and platform sent `uv` or `pinAuth`, return InvalidOption
        if !self.pin_is_set() {
            if let Some(ref options) = &options {
                if Some(true) == options.uv {
                    return Err(Error::InvalidOption);
                }
            }
            if pin_auth.is_some() {
                return Err(Error::InvalidOption);
            }
        }

        // 4. If authenticator is protected by som form of user verification, do it
        //
        // TODO: Should we should fail if `uv` is passed?
        // Current thinking: no
        if self.pin_is_set() {

            let mut uv_performed = false;
            if let Some(ref pin_auth) = pin_auth {
                if pin_auth.len() != 16 {
                    return Err(Error::InvalidParameter);
                }
                // seems a bit redundant to check here in light of 2.
                // I guess the CTAP spec writers aren't implementers :D
                if let Some(1) = pin_protocol {
                    // 5. if pinAuth is present and pinProtocol = 1, verify
                    // success --> set uv = 1
                    // error --> PinAuthInvalid
                    self.verify_pin(
                        // unwrap panic ruled out above
                        pin_auth.as_ref().try_into().unwrap(),
                        data,
                    )?;

                    return Ok(true);

                } else {
                    // 7. pinAuth present + pinProtocol != 1 --> error PinAuthInvalid
                    return Err(Error::PinAuthInvalid);
                }

            } else {
                // 6. pinAuth not present + clientPin set --> error PinRequired
                if self.pin_is_set() {
                    return Err(Error::PinRequired);
                }
            }
        }

        Ok(false)
    }

    fn locate_credentials(
        &mut self, rp_id: &String<consts::U64>,
        allow_list: &Option<ctap2::get_assertion::AllowList>,
        uv_performed: bool,
    )
        -> Result<CredentialList>
    {
        let kek = self.key_encryption_key()?;

        // validate allowList
        let allowed_credentials = if let Some(allow_list) = allow_list.as_ref() {
            let valid_allowed_credentials: CredentialList = allow_list.into_iter()
                // discard not properly serialized encrypted credentials
                .filter_map(|credential_descriptor| {
                    hprintln!("validating {:?}", &credential_descriptor).ok();
                    // hprintln!("validating").ok();
                    let esc = EncryptedSerializedCredential::try_from(CredentialId(credential_descriptor.id.clone())).ok();
                    hprintln!("step 1 passed: {}", esc.is_some()).ok();
                    esc
                    // decrypt (and thereby filter out invalid credential IDs
                    .and_then(|encrypted_credential| {
                        let ser = syscall!(self.crypto.decrypt_chacha8poly1305(
                            // TODO: use RpId as associated data here?
                            &kek,
                            &encrypted_credential.0.ciphertext,
                            &rp_id.as_bytes(),
                            &encrypted_credential.0.nonce,
                            &encrypted_credential.0.tag,
                        )).plaintext;
                        hprintln!("step 2 passed: {}", ser.is_some()).ok();
                        ser
                    })
                    .and_then(|serialized_credential| {
                        hprintln!("trying to deserialize {:?}", &serialized_credential).ok();
                        let deser = Credential::deserialize(&serialized_credential).ok();
                        hprintln!("step 3 passed: {}", deser.is_some()).ok();
                        deser
                    })
                } )
                .collect();
            if valid_allowed_credentials.len() < allow_list.len() {
                hprintln!("invalid credential").ok();
                return Err(Error::InvalidCredential);
            }
            hprintln!("allowedList passed").ok();
            valid_allowed_credentials
        } else {
            hprintln!("no allowedList passed").ok();
            CredentialList::new()
        };

        let allowed_credentials_passed = allowed_credentials.len() > 0;

        let existing_credentials: CredentialList = if allowed_credentials_passed {
            // "If an allowList is present and is non-empty,
            // locate all denoted credentials present on this authenticator
            // and bound to the specified rpId."
            allowed_credentials
                .into_iter()
                .filter(|credential| match credential.key.clone() {
                    // TODO: should check if wrapped key is valid AEAD
                    Key::WrappedKey(_) => true,
                    Key::ResidentKey(key) => {
                        match credential.algorithm {
                            -7 => syscall!(self.crypto.exists(Mechanism::P256, key)).exists,
                            -8 => syscall!(self.crypto.exists(Mechanism::Ed25519, key)).exists,
                            _ => false,
                        }
                    }
                })
                .collect()
        } else {
            // If an allowList is not present,
            // locate all credentials that are present on this authenticator
            // and bound to the specified rpId.
            todo!("implement empty allowList");
        };

        // apply credential protection policy
        let applicable_credentials: CredentialList = existing_credentials
            .into_iter()
            .filter(|credential| {
                use credential::CredentialProtectionPolicy as Policy;
                match credential.cred_protect {
                    Policy::Optional => true,
                    Policy::OptionalWithCredentialIdList => allowed_credentials_passed || uv_performed,
                    Policy::Required => uv_performed,

                }
            })
            .collect()
            ;
        //

        // "If no applicable credentials were found, return CTAP2_ERR_NO_CREDENTIALS"
        if applicable_credentials.len() == 0 {
            return Err(Error::NoCredentials);
        }

        Ok(applicable_credentials)
    }

    fn get_assertion(&mut self, parameters: &ctap2::get_assertion::Parameters) -> Result<ctap2::get_assertion::Response> {

        // 1-4.
        let uv_performed = self.pin_prechecks(
            &parameters.options, &parameters.pin_auth, &parameters.pin_protocol,
            &parameters.client_data_hash.as_ref(),
        )?;

        // 5. Locate eligible credentials
        let credentials = self.locate_credentials(&parameters.rp_id, &parameters.allow_list, uv_performed)?;
        let num_credentials = credentials.len();
        hprintln!("found {} applicable credentials", num_credentials).ok();

        hprintln!("GetAssertion not done YET").ok();
        Err(Error::InvalidCommand)
    }

    fn make_credential(&mut self, parameters: &ctap2::make_credential::Parameters) -> Result<ctap2::make_credential::Response> {

        // 1-4.
        let uv_performed = self.pin_prechecks(
            &parameters.options, &parameters.pin_auth, &parameters.pin_protocol,
            &parameters.client_data_hash.as_ref(),
        )?;

        // 5. "persist credProtect value for this credential"
        // --> seems out of place here, see 9.

        // 6. excludeList present, contains credential ID on this authenticator bound to RP?
        // --> wait for UP, error CredentialExcluded

        // 7. check pubKeyCredParams algorithm is valid + supported COSE identifier

        let mut algorithm: Option<SupportedAlgorithm> = None;
        for param in parameters.pub_key_cred_params.iter() {
            match param.alg {
                -7 => { if algorithm.is_none() { algorithm = Some(SupportedAlgorithm::P256); }}
                -8 => { algorithm = Some(SupportedAlgorithm::Ed25519); }
                _ => {}
            }
        }
        let algorithm = match algorithm {
            Some(algorithm) => algorithm,
            None => { return Err(Error::UnsupportedAlgorithm); }
        };
        // hprintln!("making credential, eddsa = {}", eddsa).ok();


        // 8. process options; on known but unsupported error UnsupportedOption

        let mut rk_requested = false;
        let mut uv_requested = false;
        let _up_requested = true; // can't be toggled

        if let Some(ref options) = &parameters.options {
            if Some(true) == options.rk {
                rk_requested = true;
            }
            if Some(true) == options.uv {
                uv_requested = true;
            }
        }

        // 9. process extensions
        // TODO: need to figure out how to type-ify these
        // let mut hmac_secret_requested = false;
        // let mut cred_protect_requested = false;
        // if let Some(extensions) = &parameters.extensions {
        //     hmac_secret_requested = match extensions.get(&String::from("hmac-secret")) {
        //         Some(true) => true,
        //         _ => false,
        //     };

        //     cred_protect_requested = match extensions.get(&String::from("credProtect")) {
        //         Some(true) => true,
        //         _ => false,
        //     };
        // }

        use ctap2::make_credential::CredentialProtectionPolicy;
        let mut hmac_secret_requested = None;
        // let mut cred_protect_requested = CredentialProtectionPolicy::Optional;
        let mut cred_protect_requested = CredentialProtectionPolicy::default();
        if let Some(extensions) = &parameters.extensions {

            if let Some(true) = extensions.hmac_secret {
                // TODO: Generate "CredRandom" (a 32B random value, to be used
                // later via HMAC-SHA256(cred_random, salt)

                let cred_random = syscall!(self.crypto.generate_hmacsha256_key(
                    StorageLocation::Internal,
                )).key;

                hmac_secret_requested = Some(match rk_requested {
                    true => {
                        CredRandom::Resident(cred_random)
                    }

                    false => {
                        let wrapping_key = &self.key_encryption_key()?;
                        let wrapped_key = syscall!(self.crypto.wrap_key_chacha8poly1305(
                            &wrapping_key,
                            &cred_random,
                            b"",
                        )).wrapped_key;

                        // 32B key, 12B nonce, 16B tag + some info on algorithm (P256/Ed25519)
                        // Turns out it's size 92 (enum serialization not optimized yet...)
                        // let mut wrapped_key = Bytes::<consts::U60>::new();
                        // wrapped_key.extend_from_slice(&wrapped_key_msg).unwrap();
                        CredRandom::Wrapped(wrapped_key.try_convert_into().map_err(|_| Error::Other)?)
                    }
                });
            }

            if let Some(policy) = &extensions.cred_protect {
                cred_protect_requested = CredentialProtectionPolicy::try_from(*policy)?;
            }
        }

        // hprintln!("hmac-secret = {:?}, credProtect = {:?}", hmac_secret_requested, cred_protect_requested).ok();

        // 10. get UP, if denied error OperationDenied
        if !self.up.user_present() {
            return Err(Error::OperationDenied);
        }

        // 11. generate credential keypair
        let location = match rk_requested {
            true => StorageLocation::Internal,
            false => StorageLocation::Volatile,
        };

        let private_key: ObjectHandle;
        let public_key: ObjectHandle;
        let cose_public_key;
        match algorithm {
            SupportedAlgorithm::P256 => {
                private_key = syscall!(self.crypto.generate_p256_private_key(location)).key;
                public_key = syscall!(self.crypto.derive_p256_public_key(&private_key, StorageLocation::Volatile)).key;
                cose_public_key = syscall!(self.crypto.serialize_key(
                    Mechanism::P256, public_key.clone(), KeySerialization::Cose
                )).serialized_key;
            }
            SupportedAlgorithm::Ed25519 => {
                private_key = syscall!(self.crypto.generate_ed25519_private_key(location)).key;
                public_key = syscall!(self.crypto.derive_ed25519_public_key(&private_key, StorageLocation::Volatile)).key;
                cose_public_key = syscall!(self.crypto.serialize_key(
                    Mechanism::Ed25519, public_key.clone(), KeySerialization::Cose
                )).serialized_key;
            }
        }

        // 12. if `rk` is set, store or overwrite key pair, if full error KeyStoreFull

        // 12.a generate credential
        let key_parameter = match rk_requested {
            true => Key::ResidentKey(private_key.clone()),
            false => {
                // WrappedKey version
                let wrapping_key = &self.key_encryption_key()?;
                let wrapped_key = syscall!(self.crypto.wrap_key_chacha8poly1305(
                    &wrapping_key,
                    &private_key,
                    b"",
                )).wrapped_key;
                // hprintln!("wrapped_key = {:?}", &wrapped_key).ok();

                // 32B key, 12B nonce, 16B tag + some info on algorithm (P256/Ed25519)
                // Turns out it's size 92 (enum serialization not optimized yet...)
                // let mut wrapped_key = Bytes::<consts::U60>::new();
                // wrapped_key.extend_from_slice(&wrapped_key_msg).unwrap();
                Key::WrappedKey(wrapped_key.try_convert_into().map_err(|_| Error::Other)?)
                // hprintln!("len wrapped key = {}", wrapped_key.len()).ok();
                // Key::WrappedKey(wrapped_key.try_convert_into().unwrap())

            }
        };

        let credential = Credential::new(
            credential::CtapVersion::Fido21Pre,
            parameters,
            -(algorithm as i32),
            key_parameter,
            123, // todo: get counter
            hmac_secret_requested.clone(),
            cred_protect_requested,
        );
        // hprintln!("credential = {:?}", &credential).ok();

        // store it.
        // TODO: overwrite, error handling with KeyStoreFull

        let serialized_credential = credential.serialize()?;
        let mut prefix = crypto_service::types::ShortData::new();
        prefix.extend_from_slice(b"rk").map_err(|_| Error::Other)?;
        let prefix = Some(crypto_service::types::Letters::try_from(prefix).map_err(|_| Error::Other)?);
        let blob_id = syscall!(self.crypto.store_blob(
            prefix.clone(),
            // credential_id.0.clone(),
            serialized_credential.clone(),
            StorageLocation::Internal,
        )).blob;


        // 12.b generate credential ID { = AEAD(Serialize(Credential)) }

        let key = &self.key_encryption_key()?;
        let message = &serialized_credential;
        let associated_data = parameters.rp.id.as_bytes();
        let encrypted_serialized_credential = EncryptedSerializedCredential(
            syscall!(self.crypto.encrypt_chacha8poly1305(key, message, associated_data)));

        // hprintln!("esc = {:?}", &encrypted_serialized_credential).ok();
        // e.g., 72B
        let credential_id: CredentialId = encrypted_serialized_credential.try_into().unwrap();
        // hprintln!("cid = {:?}", &credential_id).ok();
        // hprintln!("credential_id.len() = {}", credential_id.0.len()).ok();



        // 13. generate and return attestation statement using clientDataHash

        // 13.a AuthenticatorData and its serialization
        use ctap2::make_credential::Flags;
        let authenticator_data = ctap2::make_credential::AuthenticatorData {
            rp_id_hash: {
                let hash = syscall!(self.crypto.hash_sha256(&parameters.rp.id.as_ref())).hash;
                // Bytes::try_from_slice(&hash)
                hash.try_convert_into().map_err(|_| Error::Other)?
            },

            flags: {
                let mut flags = Flags::USER_PRESENCE;
                if uv_performed {
                    flags |= Flags::USER_VERIFIED;
                }
                if true {
                    flags |= Flags::ATTESTED_CREDENTIAL_DATA;
                }
                if hmac_secret_requested.is_some() ||  cred_protect_requested != CredentialProtectionPolicy::Optional {
                    flags |= Flags::EXTENSION_DATA;
                }
                flags
            },

            sign_count: {
                // TODO!
                123
            },

            attested_credential_data: {
                // hprintln!("acd in, cid len {}, pk len {}", credential_id.0.len(), cose_public_key.len()).ok();
                let attested_credential_data = ctap2::make_credential::AttestedCredentialData {
                    aaguid: self.config.aaguid.clone(),
                    // credential_id: credential_id.0.try_convert_into().map_err(|_| Error::Other)?,
                    // credential_public_key: cose_public_key.try_convert_into().map_err(|_| Error::Other)?,
                    credential_id: credential_id.0.try_convert_into().unwrap(),
                    credential_public_key: cose_public_key.try_convert_into().unwrap(),
                };
                // hprintln!("cose PK = {:?}", &attested_credential_data.credential_public_key).ok();
                Some(attested_credential_data)
            },

            extensions: {
                parameters.extensions.clone()
            },
        };
        // hprintln!("authData = {:?}", &authenticator_data).ok();

        let serialized_auth_data = authenticator_data.serialize();

        // 13.b The Signature

        // can we write Sum<M, N> somehow?
        // hprintln!("seeking commitment, {} + {}", serialized_auth_data.len(), parameters.client_data_hash.len()).ok();
        let mut commitment = Bytes::<consts::U1024>::new();
        commitment.extend_from_slice(&serialized_auth_data).map_err(|_| Error::Other)?;
        // hprintln!("serialized_auth_data ={:?}", &serialized_auth_data).ok();
        commitment.extend_from_slice(&parameters.client_data_hash).map_err(|_| Error::Other)?;
        // hprintln!("client_data_hash = {:?}", &parameters.client_data_hash).ok();
        // hprintln!("commitment = {:?}", &commitment).ok();

        // NB: the other/normal one is called "basic" or "batch" attestation,
        // because it attests the authenticator is part of a batch: the model
        // specified by AAGUID.
        // "self signed" is also called "surrogate basic".
        //
        // we should also directly support "none" format, it's a bit weird
        // how browsers firefox this
        const SELF_SIGNED: bool  = true;

        let (signature, attestation_algorithm) = {
            if SELF_SIGNED {
                match algorithm {
                    SupportedAlgorithm::Ed25519 => {
                        let signature = syscall!(self.crypto.sign_ed25519(&private_key, &commitment)).signature;
                        (signature.try_convert_into().map_err(|_| Error::Other)?, -8)
                    }

                    SupportedAlgorithm::P256 => {
                        // DO NOT prehash here, `crypto-service` does that
                        let der_signature = syscall!(self.crypto.sign_p256(&private_key, &commitment, SignatureSerialization::Asn1Der)).signature;
                        (der_signature.try_convert_into().map_err(|_| Error::Other)?, -7)
                    }
                }
            } else {
                let hash = syscall!(self.crypto.hash_sha256(&commitment.as_ref())).hash;
                let attestation_key = self.attestation_key()?;
                let signature = syscall!(self.crypto.sign_p256(
                    &attestation_key,
                    &hash,
                    SignatureSerialization::Asn1Der,
                )).signature;
                (signature.try_convert_into().map_err(|_| Error::Other)?, -7)
            }
        };
        // hprintln!("SIG = {:?}", &signature).ok();

        let mut packed_attn_stmt = ctap2::make_credential::PackedAttestationStatement {
            alg: attestation_algorithm,
            sig: signature,
            x5c: match SELF_SIGNED {
                true => None,
                false => {
                    // let mut x5c = Vec::new();
                    // x5c.push(Bytes::try_from_slice(&SOLO_HACKER_ATTN_CERT).unwrap()).unwrap();
                    //
                    // See: https://www.w3.org/TR/webauthn-2/#sctn-packed-attestation-cert-requirements
                    //
                    todo!("solve the cert conundrum");
                }
            },
        };

        let fmt = String::<consts::U32>::from("packed");
        let att_stmt = ctap2::make_credential::AttestationStatement::Packed(packed_attn_stmt);

        let attestation_object = ctap2::make_credential::Response {
            fmt,
            auth_data: serialized_auth_data,
            att_stmt,
        };

        Ok(attestation_object)
    }

    // fn credential_id(credential: &Credential) -> CredentialId {
    // }

    // fn get_assertion(&mut self, ...)
    //     // let unwrapped_key = syscall!(self.crypto.unwrap_key_chacha8poly1305(
    //     //     &wrapping_key,
    //     //     &wrapped_key,
    //     //     b"",
    //     //     StorageLocation::Volatile,
    //     // )).key;
        // // test public key ser/de
        // let ser_pk = syscall!(self.crypto.serialize_key(
        //     Mechanism::P256, public_key.clone(), KeySerialization::Raw
        // )).serialized_key;
        // hprintln!("ser pk = {:?}", &ser_pk).ok();

        // let cose_ser_pk = syscall!(self.crypto.serialize_key(
        //     Mechanism::P256, public_key.clone(), KeySerialization::Cose
        // )).serialized_key;
        // hprintln!("COSE ser pk = {:?}", &cose_ser_pk).ok();

        // let deser_pk = syscall!(self.crypto.deserialize_key(
        //     Mechanism::P256, ser_pk.clone(), KeySerialization::Raw,
        //     StorageAttributes::new().set_persistence(StorageLocation::Volatile)
        // )).key;
        // hprintln!("deser pk = {:?}", &deser_pk).ok();

        // let cose_deser_pk = syscall!(self.crypto.deserialize_key(
        //     Mechanism::P256, cose_ser_pk.clone(), KeySerialization::Cose,
        //     StorageAttributes::new().set_persistence(StorageLocation::Volatile)
        // )).key;
        // hprintln!("COSE deser pk = {:?}", &cose_deser_pk).ok();
        // hprintln!("raw ser of COSE deser pk = {:?}",
        //           syscall!(self.crypto.serialize_key(Mechanism::P256, cose_deser_pk.clone(), KeySerialization::Raw)).
        //           serialized_key).ok();

        // hprintln!("priv {:?}", &private_key).ok();
        // hprintln!("pub {:?}", &public_key).ok();

        // let _loaded_credential = syscall!(self.crypto.load_blob(
        //     prefix.clone(),
        //     blob_id,
        //     StorageLocation::Volatile,
        // )).data;
        // // hprintln!("loaded credential = {:?}", &loaded_credential).ok();

        // hprintln!("credential = {:?}", &Credential::deserialize(&serialized_credential)?).ok();

    //     // hprintln!("unwrapped_key = {:?}", &unwrapped_key).ok();

    fn get_info(&mut self) -> ctap2::get_info::Response {

        use core::str::FromStr;
        let mut versions = Vec::<String<consts::U12>, consts::U3>::new();
        versions.push(String::from_str("FIDO_2_1_PRE").unwrap()).unwrap();
        versions.push(String::from_str("FIDO_2_0").unwrap()).unwrap();
        versions.push(String::from_str("U2F_V2").unwrap()).unwrap();

        let mut extensions = Vec::<String<consts::U11>, consts::U4>::new();
        extensions.push(String::from_str("hmac-secret").unwrap()).unwrap();
        extensions.push(String::from_str("credProtect").unwrap()).unwrap();

        let mut pin_protocols = Vec::<u8, consts::U1>::new();
        pin_protocols.push(1).unwrap();

        let mut options = ctap2::get_info::CtapOptions::default();
        options.rk = true;
        options.up = true;
        options.uv = None; // "uv" here refers to "in itself", e.g. biometric
        // options.plat = false;
        options.client_pin = None; // not capable of PIN
        // options.client_pin = Some(true/false); // capable, is set/is not set

        ctap2::get_info::Response {
            versions,
            extensions: Some(extensions),
            aaguid: self.config.aaguid.clone(),
            options: Some(options),
            max_msg_size: Some(ctap_types::sizes::MESSAGE_SIZE),
            pin_protocols: Some(pin_protocols),
            ..ctap2::get_info::Response::default()
        }
    }

//     fn get_or_create_counter_handle(crypto_client: &mut CryptoClient) -> Result<ObjectHandle> {

//         // there should be either 0 or 1 counters with this name. if not, it's a logic error.
//         let attributes = Attributes {
//             kind: Counter,
//             label: Self::GLOBAL_COUNTER_NAME.into(),
//         };

//         // let reply = syscall!(FindObjects, attributes)?;

//         let reply = block!(
//             request::FindObjects { attributes }
//                 .submit(&mut crypto_client)
//                 // no pending requests
//                 .map_err(drop)
//                 .unwrap()
//         )?;

//         // how should this API look like.
//         match reply.num_objects() {
//             // the happy case
//             1 => Ok(reply.object_handles[0]),

//             // first run - create counter
//             0 => {
//                 let reply = block!(
//                     request::FindObjects { attributes }
//                         .submit(&mut crypto_client)
//                         // no pending requests
//                         .map_err(drop)
//                         .unwrap()
//                 )?;
//                 Ok(reply::ReadCounter::from(reply).object_handle)
//             }

//             // should not occur
//             _ => Err(Error::TooManyCounters),
//         }
//     }

//     fn get_or_create_counter_handle(crypto_client: &mut CryptoClient) -> Result<ObjectHandle> {
//         todo!("not implemented yet, follow counter code");
//     }

// }

// impl authenticator::Api for Authenticator
// {
//     fn get_info(&mut self) -> AuthenticatorInfo {
//         todo!();
//     }

//     fn reset(&mut self) -> Result<()> {
//         todo!();
//     }


//     fn get_assertions(&mut self, params: &GetAssertionParameters) -> Result<AssertionResponses> {
//         todo!();
//     }

//     fn make_credential(&mut self, params: &MakeCredentialParameters) -> Result<AttestationObject> {
//         todo!();
//     }

}

#[cfg(test)]
mod test {
}
