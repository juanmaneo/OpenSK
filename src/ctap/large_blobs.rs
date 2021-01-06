// Copyright 2020-2021 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use super::check_pin_uv_auth_protocol;
use super::command::AuthenticatorLargeBlobsParameters;
use super::pin_protocol_v1::{PinPermission, PinProtocolV1};
use super::response::{AuthenticatorLargeBlobsResponse, ResponseData};
use super::status_code::Ctap2StatusCode;
use super::storage::PersistentStore;
use alloc::vec;
use alloc::vec::Vec;
use byteorder::{ByteOrder, LittleEndian};
use crypto::sha256::Sha256;
use crypto::Hash256;

/// This is maximum message size supported by the authenticator. 1024 is the default.
/// Increasing this values can speed up commands with longer responses, but lead to
/// packets dropping or unexpected failures.
pub const MAX_MSG_SIZE: usize = 1024;
/// The length of the truncated hash that as appended to the large blob data.
const TRUNCATED_HASH_LEN: usize = 16;

pub struct LargeBlobs {
    buffer: Vec<u8>,
    expected_length: usize,
    expected_next_offset: usize,
}

/// Implements the logic for the AuthenticatorLargeBlobs command and keeps its state.
impl LargeBlobs {
    pub fn new() -> LargeBlobs {
        LargeBlobs {
            buffer: Vec::new(),
            expected_length: 0,
            expected_next_offset: 0,
        }
    }

    /// Process the large blob command.
    pub fn process_command(
        &mut self,
        persistent_store: &mut PersistentStore,
        pin_protocol_v1: &mut PinProtocolV1,
        large_blobs_params: AuthenticatorLargeBlobsParameters,
    ) -> Result<ResponseData, Ctap2StatusCode> {
        let AuthenticatorLargeBlobsParameters {
            get,
            set,
            offset,
            length,
            pin_uv_auth_param,
            pin_uv_auth_protocol,
        } = large_blobs_params;

        const MAX_FRAGMENT_LENGTH: usize = MAX_MSG_SIZE - 64;

        if let Some(get) = get {
            if get > MAX_FRAGMENT_LENGTH {
                return Err(Ctap2StatusCode::CTAP1_ERR_INVALID_LENGTH);
            }
            let config = persistent_store.get_large_blob_array(get, offset)?;
            return Ok(ResponseData::AuthenticatorLargeBlobs(Some(
                AuthenticatorLargeBlobsResponse { config },
            )));
        }

        if let Some(mut set) = set {
            if set.len() > MAX_FRAGMENT_LENGTH {
                return Err(Ctap2StatusCode::CTAP1_ERR_INVALID_LENGTH);
            }
            if offset == 0 {
                // Checks for offset and length are already done in command.
                self.expected_length =
                    length.ok_or(Ctap2StatusCode::CTAP1_ERR_INVALID_PARAMETER)?;
                self.expected_next_offset = 0;
            }
            if offset != self.expected_next_offset {
                return Err(Ctap2StatusCode::CTAP1_ERR_INVALID_SEQ);
            }
            if persistent_store.pin_hash()?.is_some() {
                let pin_uv_auth_param =
                    pin_uv_auth_param.ok_or(Ctap2StatusCode::CTAP2_ERR_PUAT_REQUIRED)?;
                // TODO(kaczmarczyck) Error codes for PIN protocol differ across commands.
                // Change to Ctap2StatusCode::CTAP2_ERR_PUAT_REQUIRED for None?
                check_pin_uv_auth_protocol(pin_uv_auth_protocol)?;
                pin_protocol_v1.has_permission(PinPermission::LargeBlobWrite)?;
                let mut message = vec![0xFF; 32];
                message.extend(&[0x0C, 0x00]);
                let mut offset_bytes = [0u8; 4];
                LittleEndian::write_u32(&mut offset_bytes, offset as u32);
                message.extend(&offset_bytes);
                message.extend(&Sha256::hash(set.as_slice()));
                if !pin_protocol_v1.verify_pin_auth_token(&message, &pin_uv_auth_param) {
                    return Err(Ctap2StatusCode::CTAP2_ERR_PIN_AUTH_INVALID);
                }
            }
            if offset + set.len() > self.expected_length {
                return Err(Ctap2StatusCode::CTAP1_ERR_INVALID_PARAMETER);
            }
            if offset == 0 {
                self.buffer = Vec::with_capacity(self.expected_length);
            }
            self.buffer.append(&mut set);
            self.expected_next_offset = self.buffer.len();
            if self.expected_next_offset == self.expected_length {
                self.expected_length = 0;
                self.expected_next_offset = 0;
                // Must be a positive number.
                let buffer_hash_index = self.buffer.len() - TRUNCATED_HASH_LEN;
                if Sha256::hash(&self.buffer[..buffer_hash_index])[..TRUNCATED_HASH_LEN]
                    != self.buffer[buffer_hash_index..]
                {
                    self.buffer = Vec::new();
                    return Err(Ctap2StatusCode::CTAP2_ERR_INTEGRITY_FAILURE);
                }
                persistent_store.commit_large_blob_array(&self.buffer)?;
                self.buffer = Vec::new();
            }
            return Ok(ResponseData::AuthenticatorLargeBlobs(None));
        }

        // This should be unreachable, since the command has either get or set.
        Err(Ctap2StatusCode::CTAP1_ERR_INVALID_PARAMETER)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crypto::rng256::ThreadRng256;

    #[test]
    fn test_process_command_get_empty() {
        let mut rng = ThreadRng256 {};
        let mut persistent_store = PersistentStore::new(&mut rng);
        let key_agreement_key = crypto::ecdh::SecKey::gensk(&mut rng);
        let pin_uv_auth_token = [0x55; 32];
        let mut pin_protocol_v1 = PinProtocolV1::new_test(key_agreement_key, pin_uv_auth_token);
        let mut large_blobs = LargeBlobs::new();

        let large_blob = vec![
            0x80, 0x76, 0xbe, 0x8b, 0x52, 0x8d, 0x00, 0x75, 0xf7, 0xaa, 0xe9, 0x8d, 0x6f, 0xa5,
            0x7a, 0x6d, 0x3c,
        ];
        let large_blobs_params = AuthenticatorLargeBlobsParameters {
            get: Some(large_blob.len()),
            set: None,
            offset: 0,
            length: None,
            pin_uv_auth_param: None,
            pin_uv_auth_protocol: None,
        };
        let large_blobs_response = large_blobs.process_command(
            &mut persistent_store,
            &mut pin_protocol_v1,
            large_blobs_params,
        );
        match large_blobs_response.unwrap() {
            ResponseData::AuthenticatorLargeBlobs(Some(response)) => {
                assert_eq!(response.config, large_blob);
            }
            _ => panic!("Invalid response type"),
        };
    }

    #[test]
    fn test_process_command_commit_and_get() {
        let mut rng = ThreadRng256 {};
        let mut persistent_store = PersistentStore::new(&mut rng);
        let key_agreement_key = crypto::ecdh::SecKey::gensk(&mut rng);
        let pin_uv_auth_token = [0x55; 32];
        let mut pin_protocol_v1 = PinProtocolV1::new_test(key_agreement_key, pin_uv_auth_token);
        let mut large_blobs = LargeBlobs::new();

        const BLOB_LEN: usize = 200;
        const DATA_LEN: usize = BLOB_LEN - TRUNCATED_HASH_LEN;
        let mut large_blob = vec![0x1B; DATA_LEN];
        large_blob.extend_from_slice(&Sha256::hash(&large_blob[..])[..TRUNCATED_HASH_LEN]);

        let large_blobs_params = AuthenticatorLargeBlobsParameters {
            get: None,
            set: Some(large_blob[..BLOB_LEN / 2].to_vec()),
            offset: 0,
            length: Some(BLOB_LEN),
            pin_uv_auth_param: None,
            pin_uv_auth_protocol: None,
        };
        let large_blobs_response = large_blobs.process_command(
            &mut persistent_store,
            &mut pin_protocol_v1,
            large_blobs_params,
        );
        assert_eq!(
            large_blobs_response,
            Ok(ResponseData::AuthenticatorLargeBlobs(None))
        );

        let large_blobs_params = AuthenticatorLargeBlobsParameters {
            get: None,
            set: Some(large_blob[BLOB_LEN / 2..].to_vec()),
            offset: BLOB_LEN / 2,
            length: None,
            pin_uv_auth_param: None,
            pin_uv_auth_protocol: None,
        };
        let large_blobs_response = large_blobs.process_command(
            &mut persistent_store,
            &mut pin_protocol_v1,
            large_blobs_params,
        );
        assert_eq!(
            large_blobs_response,
            Ok(ResponseData::AuthenticatorLargeBlobs(None))
        );

        let large_blobs_params = AuthenticatorLargeBlobsParameters {
            get: Some(BLOB_LEN),
            set: None,
            offset: 0,
            length: None,
            pin_uv_auth_param: None,
            pin_uv_auth_protocol: None,
        };
        let large_blobs_response = large_blobs.process_command(
            &mut persistent_store,
            &mut pin_protocol_v1,
            large_blobs_params,
        );
        match large_blobs_response.unwrap() {
            ResponseData::AuthenticatorLargeBlobs(Some(response)) => {
                assert_eq!(response.config, large_blob);
            }
            _ => panic!("Invalid response type"),
        };
    }

    #[test]
    fn test_process_command_commit_unexpected_offset() {
        let mut rng = ThreadRng256 {};
        let mut persistent_store = PersistentStore::new(&mut rng);
        let key_agreement_key = crypto::ecdh::SecKey::gensk(&mut rng);
        let pin_uv_auth_token = [0x55; 32];
        let mut pin_protocol_v1 = PinProtocolV1::new_test(key_agreement_key, pin_uv_auth_token);
        let mut large_blobs = LargeBlobs::new();

        const BLOB_LEN: usize = 200;
        const DATA_LEN: usize = BLOB_LEN - TRUNCATED_HASH_LEN;
        let mut large_blob = vec![0x1B; DATA_LEN];
        large_blob.extend_from_slice(&Sha256::hash(&large_blob[..])[..TRUNCATED_HASH_LEN]);

        let large_blobs_params = AuthenticatorLargeBlobsParameters {
            get: None,
            set: Some(large_blob[..BLOB_LEN / 2].to_vec()),
            offset: 0,
            length: Some(BLOB_LEN),
            pin_uv_auth_param: None,
            pin_uv_auth_protocol: None,
        };
        let large_blobs_response = large_blobs.process_command(
            &mut persistent_store,
            &mut pin_protocol_v1,
            large_blobs_params,
        );
        assert_eq!(
            large_blobs_response,
            Ok(ResponseData::AuthenticatorLargeBlobs(None))
        );

        let large_blobs_params = AuthenticatorLargeBlobsParameters {
            get: None,
            set: Some(large_blob[BLOB_LEN / 2..].to_vec()),
            // The offset is 1 too big.
            offset: BLOB_LEN / 2 + 1,
            length: None,
            pin_uv_auth_param: None,
            pin_uv_auth_protocol: None,
        };
        let large_blobs_response = large_blobs.process_command(
            &mut persistent_store,
            &mut pin_protocol_v1,
            large_blobs_params,
        );
        assert_eq!(
            large_blobs_response,
            Err(Ctap2StatusCode::CTAP1_ERR_INVALID_SEQ),
        );
    }

    #[test]
    fn test_process_command_commit_unexpected_length() {
        let mut rng = ThreadRng256 {};
        let mut persistent_store = PersistentStore::new(&mut rng);
        let key_agreement_key = crypto::ecdh::SecKey::gensk(&mut rng);
        let pin_uv_auth_token = [0x55; 32];
        let mut pin_protocol_v1 = PinProtocolV1::new_test(key_agreement_key, pin_uv_auth_token);
        let mut large_blobs = LargeBlobs::new();

        const BLOB_LEN: usize = 200;
        const DATA_LEN: usize = BLOB_LEN - TRUNCATED_HASH_LEN;
        let mut large_blob = vec![0x1B; DATA_LEN];
        large_blob.extend_from_slice(&Sha256::hash(&large_blob[..])[..TRUNCATED_HASH_LEN]);

        let large_blobs_params = AuthenticatorLargeBlobsParameters {
            get: None,
            set: Some(large_blob[..BLOB_LEN / 2].to_vec()),
            offset: 0,
            // The length is 1 too small.
            length: Some(BLOB_LEN - 1),
            pin_uv_auth_param: None,
            pin_uv_auth_protocol: None,
        };
        let large_blobs_response = large_blobs.process_command(
            &mut persistent_store,
            &mut pin_protocol_v1,
            large_blobs_params,
        );
        assert_eq!(
            large_blobs_response,
            Ok(ResponseData::AuthenticatorLargeBlobs(None))
        );

        let large_blobs_params = AuthenticatorLargeBlobsParameters {
            get: None,
            set: Some(large_blob[BLOB_LEN / 2..].to_vec()),
            offset: BLOB_LEN / 2,
            length: None,
            pin_uv_auth_param: None,
            pin_uv_auth_protocol: None,
        };
        let large_blobs_response = large_blobs.process_command(
            &mut persistent_store,
            &mut pin_protocol_v1,
            large_blobs_params,
        );
        assert_eq!(
            large_blobs_response,
            Err(Ctap2StatusCode::CTAP1_ERR_INVALID_PARAMETER),
        );
    }

    #[test]
    fn test_process_command_commit_unexpected_hash() {
        let mut rng = ThreadRng256 {};
        let mut persistent_store = PersistentStore::new(&mut rng);
        let key_agreement_key = crypto::ecdh::SecKey::gensk(&mut rng);
        let pin_uv_auth_token = [0x55; 32];
        let mut pin_protocol_v1 = PinProtocolV1::new_test(key_agreement_key, pin_uv_auth_token);
        let mut large_blobs = LargeBlobs::new();

        const BLOB_LEN: usize = 20;
        // This blob does not have an appropriate hash.
        let large_blob = vec![0x1B; BLOB_LEN];

        let large_blobs_params = AuthenticatorLargeBlobsParameters {
            get: None,
            set: Some(large_blob.to_vec()),
            offset: 0,
            length: Some(BLOB_LEN),
            pin_uv_auth_param: None,
            pin_uv_auth_protocol: None,
        };
        let large_blobs_response = large_blobs.process_command(
            &mut persistent_store,
            &mut pin_protocol_v1,
            large_blobs_params,
        );
        assert_eq!(
            large_blobs_response,
            Err(Ctap2StatusCode::CTAP2_ERR_INTEGRITY_FAILURE),
        );
    }

    #[test]
    fn test_process_command_commit_with_pin() {
        let mut rng = ThreadRng256 {};
        let mut persistent_store = PersistentStore::new(&mut rng);
        let key_agreement_key = crypto::ecdh::SecKey::gensk(&mut rng);
        let pin_uv_auth_token = [0x55; 32];
        let mut pin_protocol_v1 = PinProtocolV1::new_test(key_agreement_key, pin_uv_auth_token);
        let mut large_blobs = LargeBlobs::new();

        const BLOB_LEN: usize = 20;
        const DATA_LEN: usize = BLOB_LEN - TRUNCATED_HASH_LEN;
        let mut large_blob = vec![0x1B; DATA_LEN];
        large_blob.extend_from_slice(&Sha256::hash(&large_blob[..])[..TRUNCATED_HASH_LEN]);

        persistent_store.set_pin(&[0u8; 16], 4).unwrap();
        let pin_uv_auth_param = Some(vec![
            0x68, 0x0C, 0x3F, 0x6A, 0x62, 0x47, 0xE6, 0x7C, 0x23, 0x1F, 0x79, 0xE3, 0xDC, 0x6D,
            0xC3, 0xDE,
        ]);

        let large_blobs_params = AuthenticatorLargeBlobsParameters {
            get: None,
            set: Some(large_blob),
            offset: 0,
            length: Some(BLOB_LEN),
            pin_uv_auth_param,
            pin_uv_auth_protocol: Some(1),
        };
        let large_blobs_response = large_blobs.process_command(
            &mut persistent_store,
            &mut pin_protocol_v1,
            large_blobs_params,
        );
        assert_eq!(
            large_blobs_response,
            Ok(ResponseData::AuthenticatorLargeBlobs(None))
        );
    }
}
