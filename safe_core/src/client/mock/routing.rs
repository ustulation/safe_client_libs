// Copyright 2016 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under (1) the MaidSafe.net Commercial License,
// version 1.0 or later, or (2) The General Public License (GPL), version 3, depending on which
// licence you accepted on initial access to the Software (the "Licences").
//
// By contributing code to the SAFE Network Software, or to this project generally, you agree to be
// bound by the terms of the MaidSafe Contributor Agreement.  This, along with the Licenses can be
// found in the root directory of this project at LICENSE, COPYING and CONTRIBUTOR.
//
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.
//
// Please review the Licences for the specific language governing permissions and limitations
// relating to use of the SAFE Network Software.

use super::DataId;
use super::vault::{self, Data, Vault, VaultGuard};
use maidsafe_utilities::thread;
use rand;
use routing::{Authority, BootstrapConfig, ClientError, EntryAction, Event, FullId, ImmutableData,
              InterfaceError, MessageId, MutableData, PermissionSet, Request, Response,
              RoutingError, TYPE_TAG_SESSION_PACKET, User, XorName};
use rust_sodium::crypto::sign;
use std;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::mpsc::Sender;
use std::time::Duration;
use tiny_keccak::sha3_256;

/// Function that is used to tap into routing requests
/// and return preconditioned responsed.
pub type RequestHookFn = FnMut(&Request) -> Option<Response> + 'static;

const CONNECT_THREAD_NAME: &'static str = "Mock routing connect";
const DELAY_THREAD_NAME: &'static str = "Mock routing delay";

const DEFAULT_DELAY_MS: u64 = 0;
const CONNECT_DELAY_MS: u64 = DEFAULT_DELAY_MS;

const GET_ACCOUNT_INFO_DELAY_MS: u64 = DEFAULT_DELAY_MS;
const PUT_IDATA_DELAY_MS: u64 = DEFAULT_DELAY_MS;
const GET_IDATA_DELAY_MS: u64 = DEFAULT_DELAY_MS;

const PUT_MDATA_DELAY_MS: u64 = DEFAULT_DELAY_MS;
const GET_MDATA_VERSION_DELAY_MS: u64 = DEFAULT_DELAY_MS;
const GET_MDATA_SHELL_DELAY_MS: u64 = DEFAULT_DELAY_MS;
const GET_MDATA_DELAY_MS: u64 = DEFAULT_DELAY_MS;
const GET_MDATA_ENTRIES_DELAY_MS: u64 = DEFAULT_DELAY_MS;
const SET_MDATA_ENTRIES_DELAY_MS: u64 = DEFAULT_DELAY_MS;
const GET_MDATA_PERMISSIONS_DELAY_MS: u64 = DEFAULT_DELAY_MS;
const SET_MDATA_PERMISSIONS_DELAY_MS: u64 = DEFAULT_DELAY_MS;
const CHANGE_MDATA_OWNER_DELAY_MS: u64 = DEFAULT_DELAY_MS;

const LIST_AUTH_KEYS_AND_VERSION_DELAY_MS: u64 = DEFAULT_DELAY_MS;
const INS_AUTH_KEY_DELAY_MS: u64 = DEFAULT_DELAY_MS;
const DEL_AUTH_KEY_DELAY_MS: u64 = DEFAULT_DELAY_MS;

lazy_static! {
    static ref VAULT: Mutex<Vault> = Mutex::new(Vault::new());
}

fn lock_vault(write: bool) -> VaultGuard<'static> {
    vault::lock(&VAULT, write)
}

/// Mock routing implementation that mirrors the behaviour
/// of the real network but is not connected to it
pub struct Routing {
    sender: Sender<Event>,
    full_id: FullId,
    client_auth: Authority<XorName>,
    max_ops_countdown: Option<Cell<u64>>,
    timeout_simulation: bool,
    request_hook: Option<Box<RequestHookFn>>,
}

impl Routing {
    /// Initialises mock routing.
    /// The function signature mirrors `routing::Client`.
    pub fn new(
        sender: Sender<Event>,
        id: Option<FullId>,
        _config: Option<BootstrapConfig>,
        _msg_expiry_dur: Duration,
    ) -> Result<Self, RoutingError> {
        ::rust_sodium::init();

        let cloned_sender = sender.clone();
        let _ = thread::named(CONNECT_THREAD_NAME, move || {
            std::thread::sleep(Duration::from_millis(CONNECT_DELAY_MS));
            let _ = cloned_sender.send(Event::Connected);
        });

        let client_auth = Authority::Client {
            client_id: *FullId::new().public_id(),
            proxy_node_name: rand::random(),
        };

        Ok(Routing {
            sender: sender,
            full_id: id.unwrap_or_else(FullId::new),
            client_auth: client_auth,
            max_ops_countdown: None,
            timeout_simulation: false,
            request_hook: None,
        })
    }

    /// Gets MAID account information.
    pub fn get_account_info(
        &mut self,
        dst: Authority<XorName>,
        msg_id: MessageId,
    ) -> Result<(), InterfaceError> {
        if self.simulate_network_errors() {
            return Ok(());
        }

        let res = if let Err(err) = self.verify_network_limits(msg_id, "get_account_info") {
            Err(err)
        } else {
            let name = match dst {
                Authority::ClientManager(name) => name,
                x => panic!("Unexpected authority: {:?}", x),
            };

            let vault = lock_vault(false);
            match vault.get_account(&name) {
                Some(account) => Ok(*account.account_info()),
                None => Err(ClientError::NoSuchAccount),
            }
        };

        self.send_response(
            GET_ACCOUNT_INFO_DELAY_MS,
            dst,
            self.client_auth,
            Response::GetAccountInfo {
                res: res,
                msg_id: msg_id,
            },
        );

        Ok(())
    }

    /// Puts ImmutableData to the network.
    pub fn put_idata(
        &mut self,
        dst: Authority<XorName>,
        data: ImmutableData,
        msg_id: MessageId,
    ) -> Result<(), InterfaceError> {
        let data_name = *data.name();
        let nae_auth = Authority::NaeManager(data_name);

        let override_response = if let Some(ref mut hook) = self.request_hook {
            hook(&Request::PutIData {
                data: data.clone(),
                msg_id,
            })
        } else {
            None
        };
        if let Some(response) = override_response {
            self.send_response(PUT_IDATA_DELAY_MS, nae_auth, self.client_auth, response);
            return Ok(());
        }

        if self.simulate_network_errors() {
            return Ok(());
        }

        let mut vault = lock_vault(true);

        let res = {
            self.verify_network_limits(msg_id, "put_idata")
                .and_then(|_| vault.authorise_mutation(&dst, self.client_key()))
                .and_then(|_| {
                    match vault.get_data(&DataId::immutable(*data.name())) {
                        // Immutable data is de-duplicated so always allowed
                        Some(Data::Immutable(_)) => Ok(()),
                        Some(_) => Err(ClientError::DataExists),
                        None => {
                            vault.insert_data(DataId::immutable(data_name), Data::Immutable(data));
                            Ok(())
                        }
                    }
                })
                .map(|_| vault.commit_mutation(&dst))
        };

        self.send_response(
            PUT_IDATA_DELAY_MS,
            nae_auth,
            self.client_auth,
            Response::PutIData { res, msg_id },
        );
        Ok(())
    }

    /// Fetches ImmutableData from the network by the given name.
    pub fn get_idata(
        &mut self,
        dst: Authority<XorName>,
        name: XorName,
        msg_id: MessageId,
    ) -> Result<(), InterfaceError> {
        let nae_auth = Authority::NaeManager(name);

        let override_response = if let Some(ref mut hook) = self.request_hook {
            hook(&Request::GetIData { name, msg_id })
        } else {
            None
        };
        if let Some(response) = override_response {
            self.send_response(GET_IDATA_DELAY_MS, nae_auth, self.client_auth, response);
            return Ok(());
        }

        if self.simulate_network_errors() {
            return Ok(());
        }

        let vault = lock_vault(false);

        let res = if let Err(err) = self.verify_network_limits(msg_id, "get_idata") {
            Err(err)
        } else if let Err(err) = vault.authorise_read(&dst, &name) {
            Err(err)
        } else {
            match vault.get_data(&DataId::immutable(name)) {
                Some(Data::Immutable(data)) => Ok(data),
                _ => Err(ClientError::NoSuchData),
            }
        };

        self.send_response(
            GET_IDATA_DELAY_MS,
            nae_auth,
            self.client_auth,
            Response::GetIData { res, msg_id },
        );
        Ok(())
    }

    /// Creates a new MutableData in the network.
    pub fn put_mdata(
        &mut self,
        dst: Authority<XorName>,
        data: MutableData,
        msg_id: MessageId,
        requester: sign::PublicKey,
    ) -> Result<(), InterfaceError> {
        let data_name = DataId::mutable(*data.name(), data.tag());
        let nae_auth = Authority::NaeManager(*data_name.name());

        let override_response = if let Some(ref mut hook) = self.request_hook {
            hook(&Request::PutMData {
                data: data.clone(),
                msg_id,
                requester,
            })
        } else {
            None
        };
        if let Some(response) = override_response {
            self.send_response(PUT_MDATA_DELAY_MS, nae_auth, self.client_auth, response);
            return Ok(());
        }

        if self.simulate_network_errors() {
            return Ok(());
        }

        let mut vault = lock_vault(true);

        let res = if let Err(err) = self.verify_network_limits(msg_id, "put_mdata") {
            Err(err)
        } else if data.tag() == TYPE_TAG_SESSION_PACKET {
            // Put Account.
            let dst_name = match dst {
                Authority::ClientManager(name) => name,
                x => panic!("Unexpected authority: {:?}", x),
            };

            if vault.contains_data(&data_name) {
                Err(ClientError::AccountExists)
            } else {
                vault.insert_account(dst_name);
                vault.insert_data(data_name, Data::Mutable(data));
                Ok(())
            }
        } else {
            // Put normal data.
            vault
                .authorise_mutation(&dst, self.client_key())
                .and_then(|_| Self::verify_owner(&dst, data.owners()))
                .and_then(|_| if vault.contains_data(&data_name) {
                    Err(ClientError::DataExists)
                } else {
                    vault.insert_data(data_name, Data::Mutable(data));
                    Ok(())
                })
                .map(|_| vault.commit_mutation(&dst))
        };

        self.send_response(
            PUT_MDATA_DELAY_MS,
            nae_auth,
            self.client_auth,
            Response::PutMData { res, msg_id },
        );
        Ok(())
    }

    /// Fetches a latest version number.
    pub fn get_mdata_version(
        &mut self,
        dst: Authority<XorName>,
        name: XorName,
        tag: u64,
        msg_id: MessageId,
    ) -> Result<(), InterfaceError> {
        self.read_mdata(dst,
                        name,
                        tag,
                        Request::GetMDataVersion { name, tag, msg_id },
                        "get_mdata_version",
                        GET_MDATA_VERSION_DELAY_MS,
                        |data| Ok(data.version()),
                        |res| Response::GetMDataVersion { res, msg_id })
    }

    /// Fetches a complete MutableData object.
    pub fn get_mdata(
        &mut self,
        dst: Authority<XorName>,
        name: XorName,
        tag: u64,
        msg_id: MessageId,
    ) -> Result<(), InterfaceError> {
        self.read_mdata(dst,
                        name,
                        tag,
                        Request::GetMData { name, tag, msg_id },
                        "get_mdata",
                        GET_MDATA_DELAY_MS,
                        Ok,
                        |res| Response::GetMData { res, msg_id })
    }

    /// Fetches a shell of given MutableData.
    pub fn get_mdata_shell(
        &mut self,
        dst: Authority<XorName>,
        name: XorName,
        tag: u64,
        msg_id: MessageId,
    ) -> Result<(), InterfaceError> {
        self.read_mdata(dst,
                        name,
                        tag,
                        Request::GetMDataShell { name, tag, msg_id },
                        "get_mdata_shell",
                        GET_MDATA_SHELL_DELAY_MS,
                        |data| Ok(data.shell()),
                        |res| Response::GetMDataShell { res, msg_id })
    }

    /// Fetches a list of entries (keys + values).
    pub fn list_mdata_entries(
        &mut self,
        dst: Authority<XorName>,
        name: XorName,
        tag: u64,
        msg_id: MessageId,
    ) -> Result<(), InterfaceError> {
        self.read_mdata(dst,
                        name,
                        tag,
                        Request::ListMDataEntries { name, tag, msg_id },
                        "list_mdata_entries",
                        GET_MDATA_ENTRIES_DELAY_MS,
                        |data| Ok(data.entries().clone()),
                        |res| Response::ListMDataEntries { res, msg_id })
    }

    /// Fetches a list of keys in MutableData.
    pub fn list_mdata_keys(
        &mut self,
        dst: Authority<XorName>,
        name: XorName,
        tag: u64,
        msg_id: MessageId,
    ) -> Result<(), InterfaceError> {
        self.read_mdata(dst,
                        name,
                        tag,
                        Request::ListMDataKeys { name, tag, msg_id },
                        "list_mdata_keys",
                        GET_MDATA_ENTRIES_DELAY_MS,
                        |data| {
                            let keys = data.keys().into_iter().cloned().collect();
                            Ok(keys)
                        },
                        |res| Response::ListMDataKeys { res, msg_id })
    }

    /// Fetches a list of values in MutableData.
    pub fn list_mdata_values(
        &mut self,
        dst: Authority<XorName>,
        name: XorName,
        tag: u64,
        msg_id: MessageId,
    ) -> Result<(), InterfaceError> {
        self.read_mdata(dst,
                        name,
                        tag,
                        Request::ListMDataValues { name, tag, msg_id },
                        "list_mdata_values",
                        GET_MDATA_ENTRIES_DELAY_MS,
                        |data| {
                            let values = data.values().into_iter().cloned().collect();
                            Ok(values)
                        },
                        |res| Response::ListMDataValues { res, msg_id })
    }

    /// Fetches a single value from MutableData
    pub fn get_mdata_value(
        &mut self,
        dst: Authority<XorName>,
        name: XorName,
        tag: u64,
        key: Vec<u8>,
        msg_id: MessageId,
    ) -> Result<(), InterfaceError> {
        self.read_mdata(dst,
                        name,
                        tag,
                        Request::GetMDataValue {
                            name,
                            tag,
                            key: key.clone(),
                            msg_id,
                        },
                        "get_mdata_value",
                        GET_MDATA_ENTRIES_DELAY_MS,
                        |data| data.get(&key).cloned().ok_or(ClientError::NoSuchEntry),
                        |res| Response::GetMDataValue { res, msg_id })
    }

    /// Updates MutableData entries in bulk.
    pub fn mutate_mdata_entries(
        &mut self,
        dst: Authority<XorName>,
        name: XorName,
        tag: u64,
        actions: BTreeMap<Vec<u8>, EntryAction>,
        msg_id: MessageId,
        requester: sign::PublicKey,
    ) -> Result<(), InterfaceError> {
        let actions2 = actions.clone();

        self.mutate_mdata(dst,
                          name,
                          tag,
                          Request::MutateMDataEntries {
                              name,
                              tag,
                              msg_id,
                              actions,
                              requester,
                          },
                          requester,
                          "mutate_mdata_entries",
                          SET_MDATA_ENTRIES_DELAY_MS,
                          |data| data.mutate_entries(actions2, requester),
                          |res| Response::MutateMDataEntries { res, msg_id })
    }

    /// Fetches a complete list of permissions.
    pub fn list_mdata_permissions(
        &mut self,
        dst: Authority<XorName>,
        name: XorName,
        tag: u64,
        msg_id: MessageId,
    ) -> Result<(), InterfaceError> {
        self.read_mdata(dst,
                        name,
                        tag,
                        Request::ListMDataPermissions { name, tag, msg_id },
                        "list_mdata_permissions",
                        GET_MDATA_PERMISSIONS_DELAY_MS,
                        |data| Ok(data.permissions().clone()),
                        |res| Response::ListMDataPermissions { res, msg_id })
    }

    /// Fetches a list of permissions for a particular User.
    pub fn list_mdata_user_permissions(
        &mut self,
        dst: Authority<XorName>,
        name: XorName,
        tag: u64,
        user: User,
        msg_id: MessageId,
    ) -> Result<(), InterfaceError> {
        self.read_mdata(dst,
                        name,
                        tag,
                        Request::ListMDataUserPermissions {
                            name,
                            tag,
                            user,
                            msg_id,
                        },
                        "list_mdata_user_permissions",
                        GET_MDATA_PERMISSIONS_DELAY_MS,
                        |data| data.user_permissions(&user).map(|p| *p),
                        |res| Response::ListMDataUserPermissions { res, msg_id })
    }

    /// Updates or inserts a list of permissions for a particular User in the given
    /// MutableData.
    pub fn set_mdata_user_permissions(
        &mut self,
        dst: Authority<XorName>,
        name: XorName,
        tag: u64,
        user: User,
        permissions: PermissionSet,
        version: u64,
        msg_id: MessageId,
        requester: sign::PublicKey,
    ) -> Result<(), InterfaceError> {
        self.mutate_mdata(dst,
                          name,
                          tag,
                          Request::SetMDataUserPermissions {
                              name,
                              tag,
                              user,
                              permissions,
                              version,
                              msg_id,
                              requester,
                          },
                          requester,
                          "set_mdata_user_permissions",
                          SET_MDATA_PERMISSIONS_DELAY_MS,
                          |data| data.set_user_permissions(user, permissions, version, requester),
                          |res| Response::SetMDataUserPermissions { res, msg_id })
    }

    /// Deletes a list of permissions for a particular User in the given MutableData.
    pub fn del_mdata_user_permissions(
        &mut self,
        dst: Authority<XorName>,
        name: XorName,
        tag: u64,
        user: User,
        version: u64,
        msg_id: MessageId,
        requester: sign::PublicKey,
    ) -> Result<(), InterfaceError> {
        self.mutate_mdata(dst,
                          name,
                          tag,
                          Request::DelMDataUserPermissions {
                              name,
                              tag,
                              user,
                              version,
                              msg_id,
                              requester,
                          },
                          requester,
                          "del_mdata_user_permissions",
                          SET_MDATA_PERMISSIONS_DELAY_MS,
                          |data| data.del_user_permissions(&user, version, requester),
                          |res| Response::DelMDataUserPermissions { res, msg_id })
    }

    /// Changes an owner of the given MutableData. Only the current owner can perform this action.
    pub fn change_mdata_owner(
        &mut self,
        dst: Authority<XorName>,
        name: XorName,
        tag: u64,
        new_owners: BTreeSet<sign::PublicKey>,
        version: u64,
        msg_id: MessageId,
    ) -> Result<(), InterfaceError> {
        let new_owners_len = new_owners.len();
        let new_owner = match new_owners.into_iter().next() {
            Some(owner) if new_owners_len == 1 => owner,
            Some(_) | None => {
                // `new_owners` must have exactly 1 element.
                self.send_response(
                    CHANGE_MDATA_OWNER_DELAY_MS,
                    dst,
                    self.client_auth,
                    Response::ChangeMDataOwner {
                        res: Err(ClientError::InvalidOwners),
                        msg_id,
                    },
                );
                return Ok(());
            }
        };

        let requester = *self.client_key();
        let requester_name = XorName(sha3_256(&requester[..]));

        self.mutate_mdata(dst,
                          name,
                          tag,
                          Request::ChangeMDataOwner {
                              name,
                              tag,
                              new_owners: btree_set![new_owner],
                              version,
                              msg_id,
                          },
                          requester,
                          "change_mdata_owner",
                          CHANGE_MDATA_OWNER_DELAY_MS,
                          |data| {
            let dst_name = match dst {
                Authority::ClientManager(name) => name,
                _ => return Err(ClientError::InvalidOwners),
            };

            // Only the current owner can change ownership for MD
            match Self::verify_owner(&dst, data.owners()) {
                Err(ClientError::InvalidOwners) => return Err(ClientError::AccessDenied),
                Err(e) => return Err(e),
                Ok(_) => (),
            }

            if requester_name != dst_name {
                Err(ClientError::AccessDenied)
            } else {
                data.change_owner(new_owner, version)
            }
        },
                          |res| Response::ChangeMDataOwner { res, msg_id })
    }

    /// Fetches a list of authorised keys and version in MaidManager
    pub fn list_auth_keys_and_version(
        &mut self,
        dst: Authority<XorName>,
        msg_id: MessageId,
    ) -> Result<(), InterfaceError> {
        let override_response = if let Some(ref mut hook) = self.request_hook {
            hook(&Request::ListAuthKeysAndVersion(msg_id))
        } else {
            None
        };
        if let Some(response) = override_response {
            self.send_response(
                LIST_AUTH_KEYS_AND_VERSION_DELAY_MS,
                dst,
                self.client_auth,
                response,
            );
            return Ok(());
        }

        if self.simulate_network_errors() {
            return Ok(());
        }

        let res =
            if let Err(err) = self.verify_network_limits(msg_id, "list_auth_keys_and_version") {
                Err(err)
            } else {
                let name = match dst {
                    Authority::ClientManager(name) => name,
                    x => panic!("Unexpected authority: {:?}", x),
                };

                let vault = lock_vault(false);
                if let Some(account) = vault.get_account(&name) {
                    Ok((account.auth_keys().clone(), account.version()))
                } else {
                    Err(ClientError::NoSuchAccount)
                }
            };

        self.send_response(
            LIST_AUTH_KEYS_AND_VERSION_DELAY_MS,
            dst,
            self.client_auth,
            Response::ListAuthKeysAndVersion { res, msg_id },
        );
        Ok(())
    }

    /// Adds a new authorised key to MaidManager
    pub fn ins_auth_key(
        &mut self,
        dst: Authority<XorName>,
        key: sign::PublicKey,
        version: u64,
        msg_id: MessageId,
    ) -> Result<(), InterfaceError> {
        let override_response = if let Some(ref mut hook) = self.request_hook {
            hook(&Request::InsAuthKey {
                key,
                version,
                msg_id,
            })
        } else {
            None
        };
        if let Some(response) = override_response {
            self.send_response(INS_AUTH_KEY_DELAY_MS, dst, self.client_auth, response);
            return Ok(());
        }

        if self.simulate_network_errors() {
            return Ok(());
        }

        let res = if let Err(err) = self.verify_network_limits(msg_id, "ins_auth_key") {
            Err(err)
        } else {
            let name = match dst {
                Authority::ClientManager(name) => name,
                x => panic!("Unexpected authority: {:?}", x),
            };

            let mut vault = lock_vault(true);
            if let Some(account) = vault.get_account_mut(&name) {
                account.ins_auth_key(key, version)
            } else {
                Err(ClientError::NoSuchAccount)
            }
        };


        self.send_response(
            INS_AUTH_KEY_DELAY_MS,
            dst,
            self.client_auth,
            Response::InsAuthKey { res, msg_id },
        );
        Ok(())
    }

    /// Removes an authorised key from MaidManager
    pub fn del_auth_key(
        &mut self,
        dst: Authority<XorName>,
        key: sign::PublicKey,
        version: u64,
        msg_id: MessageId,
    ) -> Result<(), InterfaceError> {
        let override_response = if let Some(ref mut hook) = self.request_hook {
            hook(&Request::DelAuthKey {
                key,
                version,
                msg_id,
            })
        } else {
            None
        };
        if let Some(response) = override_response {
            self.send_response(DEL_AUTH_KEY_DELAY_MS, dst, self.client_auth, response);
            return Ok(());
        }

        if self.simulate_network_errors() {
            return Ok(());
        }

        let res = if let Err(err) = self.verify_network_limits(msg_id, "del_auth_key") {
            Err(err)
        } else {
            let name = match dst {
                Authority::ClientManager(name) => name,
                x => panic!("Unexpected authority: {:?}", x),
            };

            let mut vault = lock_vault(true);
            if let Some(account) = vault.get_account_mut(&name) {
                account.del_auth_key(&key, version)
            } else {
                Err(ClientError::NoSuchAccount)
            }
        };

        self.send_response(
            DEL_AUTH_KEY_DELAY_MS,
            dst,
            self.client_auth,
            Response::DelAuthKey { res, msg_id },
        );
        Ok(())
    }

    fn send_response(
        &self,
        delay_ms: u64,
        src: Authority<XorName>,
        dst: Authority<XorName>,
        response: Response,
    ) {
        let event = Event::Response {
            response: response,
            src: src,
            dst: dst,
        };

        self.send_event(delay_ms, event)
    }

    fn send_event(&self, delay_ms: u64, event: Event) {
        if delay_ms > 0 {
            let sender = self.sender.clone();
            let _ = thread::named(DELAY_THREAD_NAME, move || {
                std::thread::sleep(Duration::from_millis(delay_ms));
                if let Err(err) = sender.send(event) {
                    error!("mpsc-send failure: {:?}", err);
                }
            });
        } else if let Err(err) = self.sender.send(event) {
            error!("mpsc-send failure: {:?}", err);
        }
    }

    fn client_name(&self) -> XorName {
        match self.client_auth {
            Authority::Client { ref client_id, .. } => *client_id.name(),
            _ => panic!("This authority must be Client"),
        }
    }

    fn read_mdata<F, G, R>(
        &mut self,
        dst: Authority<XorName>,
        name: XorName,
        tag: u64,
        request: Request,
        log_label: &str,
        delay_ms: u64,
        f: F,
        g: G,
    ) -> Result<(), InterfaceError>
    where
        F: FnOnce(MutableData) -> Result<R, ClientError>,
        G: FnOnce(Result<R, ClientError>) -> Response,
    {
        self.with_mdata(
            name,
            tag,
            request,
            None,
            log_label,
            delay_ms,
            false,
            |data, vault| {
                vault.authorise_read(&dst, &name)?;
                f(data)
            },
            g,
        )
    }

    fn mutate_mdata<F, G, R>(
        &mut self,
        dst: Authority<XorName>,
        name: XorName,
        tag: u64,
        request: Request,
        requester: sign::PublicKey,
        log_label: &str,
        delay_ms: u64,
        f: F,
        g: G,
    ) -> Result<(), InterfaceError>
    where
        F: FnOnce(&mut MutableData) -> Result<R, ClientError>,
        G: FnOnce(Result<R, ClientError>) -> Response,
    {
        let client_key = *self.client_key();
        let mutate = |mut data: MutableData, vault: &mut Vault| {
            vault.authorise_mutation(&dst, &client_key)?;

            let output = f(&mut data)?;
            vault.insert_data(DataId::mutable(name, tag), Data::Mutable(data));
            vault.commit_mutation(&dst);

            Ok(output)
        };

        self.with_mdata(
            name,
            tag,

            request,
            Some(requester),
            log_label,
            delay_ms,
            true,
            mutate,
            g,
        )
    }

    fn with_mdata<F, G, R>(
        &mut self,
        name: XorName,
        tag: u64,
        request: Request,
        requester: Option<sign::PublicKey>,
        log_label: &str,
        delay_ms: u64,
        write: bool,
        f: F,
        g: G,
    ) -> Result<(), InterfaceError>
    where
        F: FnOnce(MutableData, &mut Vault) -> Result<R, ClientError>,
        G: FnOnce(Result<R, ClientError>) -> Response,
    {
        let nae_auth = Authority::NaeManager(name);
        let msg_id = *request.message_id();

        let override_response = if let Some(ref mut hook) = self.request_hook {
            hook(&request)
        } else {
            None
        };
        if let Some(response) = override_response {
            self.send_response(delay_ms, nae_auth, self.client_auth, response);
            return Ok(());
        };

        if self.simulate_network_errors() {
            return Ok(());
        }

        let res = if let Err(err) = self.verify_network_limits(msg_id, log_label) {
            Err(err)
        } else if let Err(err) = self.verify_requester(requester) {
            Err(err)
        } else {
            let mut vault = lock_vault(write);
            match vault.get_data(&DataId::mutable(name, tag)) {
                Some(Data::Mutable(data)) => f(data, &mut *vault),
                _ => {
                    if tag == TYPE_TAG_SESSION_PACKET {
                        Err(ClientError::NoSuchAccount)
                    } else {
                        Err(ClientError::NoSuchData)
                    }
                }
            }
        };

        self.send_response(delay_ms, nae_auth, self.client_auth, g(res));
        Ok(())
    }

    fn verify_owner(
        dst: &Authority<XorName>,
        owner_keys: &BTreeSet<sign::PublicKey>,
    ) -> Result<(), ClientError> {
        let dst_name = match *dst {
            Authority::ClientManager(name) => name,
            _ => return Err(ClientError::InvalidOwners),
        };

        let ok = owner_keys.iter().any(|owner_key| {
            let owner_name = XorName(sha3_256(&owner_key.0));
            owner_name == dst_name
        });

        if ok {
            Ok(())
        } else {
            Err(ClientError::InvalidOwners)
        }
    }

    fn verify_requester(&self, requester: Option<sign::PublicKey>) -> Result<(), ClientError> {
        let requester = match requester {
            Some(key) => key,
            None => return Ok(()),
        };

        if *self.client_key() == requester {
            Ok(())
        } else {
            Err(ClientError::from("Invalid requester"))
        }
    }

    /// Returns the default boostrap config
    pub fn bootstrap_config() -> Result<BootstrapConfig, InterfaceError> {
        Ok(BootstrapConfig::default())
    }

    fn verify_network_limits(&self, msg_id: MessageId, op: &str) -> Result<(), ClientError> {
        let client_name = self.client_name();

        if self.network_limits_reached() {
            info!("Mock {}: {:?} {:?} [0]", op, client_name, msg_id);
            Err(ClientError::NetworkOther(
                "Max operations exhausted".to_string(),
            ))
        } else {
            if let Some(count) = self.update_network_limits() {
                info!("Mock {}: {:?} {:?} [{}]", op, client_name, msg_id, count);
            }

            Ok(())
        }
    }

    fn network_limits_reached(&self) -> bool {
        self.max_ops_countdown.as_ref().map_or(
            false,
            |count| count.get() == 0,
        )
    }

    fn update_network_limits(&self) -> Option<u64> {
        self.max_ops_countdown.as_ref().map(|count| {
            let ops = count.get();
            count.set(ops - 1);
            ops
        })
    }

    fn simulate_network_errors(&self) -> bool {
        if self.timeout_simulation {
            return true;
        }

        false
    }

    fn client_key(&self) -> &sign::PublicKey {
        self.full_id.public_id().signing_public_key()
    }
}

#[cfg(any(feature = "testing", test))]
impl Routing {
    /// Set hook function to override response results for test purposes.
    pub fn set_request_hook<F>(&mut self, hook: F)
    where
        F: FnMut(&Request) -> Option<Response> + 'static,
    {
        let hook: Box<RequestHookFn> = Box::new(hook);
        self.request_hook = Some(hook);
    }

    /// Removes hook function to override response results
    pub fn remove_request_hook(&mut self) {
        self.request_hook = None;
    }

    /// Sets a maximum number of operations
    pub fn set_network_limits(&mut self, max_ops_count: Option<u64>) {
        self.max_ops_countdown = max_ops_count.map(Cell::new)
    }

    /// Simulates network disconnect
    pub fn simulate_disconnect(&self) {
        let sender = self.sender.clone();
        let _ = std::thread::spawn(move || unwrap!(sender.send(Event::Terminate)));
    }

    /// Simulates network timeouts
    pub fn set_simulate_timeout(&mut self, enable: bool) {
        self.timeout_simulation = enable;
    }
}

impl Drop for Routing {
    fn drop(&mut self) {
        let _ = self.sender.send(Event::Terminate);
    }
}
