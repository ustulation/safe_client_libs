// Copyright 2016 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under (1) the MaidSafe.net
// Commercial License, version 1.0 or later, or (2) The General Public License
// (GPL), version 3, depending on which licence you accepted on initial access
// to the Software (the "Licences").
//
// By contributing code to the SAFE Network Software, or to this project
// generally, you agree to be bound by the terms of the MaidSafe Contributor
// Agreement, version 1.0.
// This, along with the Licenses can be found in the root directory of this
// project at LICENSE, COPYING and CONTRIBUTOR.
//
// Unless required by applicable law or agreed to in writing, the SAFE Network
// Software distributed under the GPL Licence is distributed on an "AS IS"
// BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
// implied.
//
// Please review the Licences for the specific language governing permissions
// and limitations relating to use of the SAFE Network Software.

use ffi::{MDataEntryActionsHandle, OpaqueCtx, Session, helper};
use routing::{EntryAction, Value};
use std::os::raw::c_void;

/// Create new entry actions.
#[no_mangle]
pub unsafe extern "C"
fn mdata_entry_actions_new(session: *const Session,
                           user_data: *mut c_void,
                           o_cb: unsafe extern "C" fn(*mut c_void,
                                                      i32,
                                                      MDataEntryActionsHandle)) {
    helper::catch_unwind_cb(user_data, o_cb, || {
        let user_data = OpaqueCtx(user_data);

        (*session).send(move |_, object_cache| {
            let actions = Default::default();
            let handle = object_cache.insert_mdata_entry_actions(actions);

            o_cb(user_data.0, 0, handle);
            None
        })
    })
}

/// Add action to insert new entry.
#[no_mangle]
pub unsafe extern "C" fn mdata_entry_actions_insert(session: *const Session,
                                                    actions_h: MDataEntryActionsHandle,
                                                    key_ptr: *const u8,
                                                    key_len: usize,
                                                    value_ptr: *const u8,
                                                    value_len: usize,
                                                    user_data: *mut c_void,
                                                    o_cb: unsafe extern "C" fn(*mut c_void, i32)) {
    add_action(session, actions_h, key_ptr, key_len, user_data, o_cb, || {
        EntryAction::Ins(Value {
            content: helper::u8_ptr_to_vec(value_ptr, value_len),
            entry_version: 0,
        })
    })
}

/// Add action to update existing entry.
#[no_mangle]
pub unsafe extern "C" fn mdata_entry_actions_update(session: *const Session,
                                                    actions_h: MDataEntryActionsHandle,
                                                    key_ptr: *const u8,
                                                    key_len: usize,
                                                    value_ptr: *const u8,
                                                    value_len: usize,
                                                    entry_version: u64,
                                                    user_data: *mut c_void,
                                                    o_cb: unsafe extern "C" fn(*mut c_void, i32)) {
    add_action(session, actions_h, key_ptr, key_len, user_data, o_cb, || {
        EntryAction::Update(Value {
            content: helper::u8_ptr_to_vec(value_ptr, value_len),
            entry_version: entry_version,
        })
    })
}

/// Add action to delete existing entry.
#[no_mangle]
pub unsafe extern "C" fn mdata_entry_actions_delete(session: *const Session,
                                                    actions_h: MDataEntryActionsHandle,
                                                    key_ptr: *const u8,
                                                    key_len: usize,
                                                    entry_version: u64,
                                                    user_data: *mut c_void,
                                                    o_cb: unsafe extern "C" fn(*mut c_void, i32)) {
    add_action(session,
               actions_h,
               key_ptr,
               key_len,
               user_data,
               o_cb,
               || EntryAction::Del(entry_version))
}

/// Free the entry actions from memory
#[no_mangle]
pub unsafe extern "C" fn mdata_entry_actions_free(session: *const Session,
                                                  actions_h: MDataEntryActionsHandle,
                                                  user_data: *mut c_void,
                                                  o_cb: unsafe extern "C" fn(*mut c_void, i32)) {
    helper::catch_unwind_cb(user_data, o_cb, || {
        let user_data = OpaqueCtx(user_data);

        (*session).send(move |_, object_cache| {
            let _ = try_cb!(object_cache.remove_mdata_entry_actions(actions_h),
                            user_data,
                            o_cb);

            o_cb(user_data.0, 0);
            None
        })
    })
}

// Add new action to the entry actions stored in the object cache. The action
// to add is the result of the passed in lambda `f`.
unsafe fn add_action<F>(session: *const Session,
                        actions_h: MDataEntryActionsHandle,
                        key_ptr: *const u8,
                        key_len: usize,
                        user_data: *mut c_void,
                        o_cb: unsafe extern "C" fn(*mut c_void, i32),
                        f: F)
    where F: FnOnce() -> EntryAction
{
    helper::catch_unwind_cb(user_data, o_cb, || {
        let user_data = OpaqueCtx(user_data);
        let key = helper::u8_ptr_to_vec(key_ptr, key_len);
        let action = f();

        (*session).send(move |_, object_cache| {
            let mut actions = try_cb!(object_cache.get_mdata_entry_actions(actions_h),
                                      user_data,
                                      o_cb);
            let _ = actions.insert(key, action);

            o_cb(user_data.0, 0);
            None
        })
    })
}

#[cfg(test)]
mod tests {
    use core::utility;
    use ffi::test_utils;
    use routing::{EntryAction, Value};
    use super::*;

    #[test]
    fn basics() {
        let session = test_utils::create_session();

        let handle = unsafe {
            unwrap!(test_utils::call_1(|ud, cb| mdata_entry_actions_new(&session, ud, cb)))
        };

        test_utils::run_now(&session, move |_, object_cache| {
            let actions = unwrap!(object_cache.get_mdata_entry_actions(handle));
            assert!(actions.is_empty());
        });

        let key0 = b"key0".to_vec();
        let key1 = b"key1".to_vec();
        let key2 = b"key2".to_vec();

        let value0 = unwrap!(utility::generate_random_vector(10));
        let value1 = unwrap!(utility::generate_random_vector(10));

        let version1 = 4;
        let version2 = 8;

        unsafe {
            unwrap!(test_utils::call_0(|ud, cb| {
                mdata_entry_actions_insert(&session,
                                           handle,
                                           key0.as_ptr(),
                                           key0.len(),
                                           value0.as_ptr(),
                                           value0.len(),
                                           ud,
                                           cb)
            }));

            unwrap!(test_utils::call_0(|ud, cb| {
                mdata_entry_actions_update(&session,
                                           handle,
                                           key1.as_ptr(),
                                           key1.len(),
                                           value1.as_ptr(),
                                           value1.len(),
                                           version1,
                                           ud,
                                           cb)
            }));

            unwrap!(test_utils::call_0(|ud, cb| {
                mdata_entry_actions_delete(&session,
                                           handle,
                                           key2.as_ptr(),
                                           key2.len(),
                                           version2,
                                           ud,
                                           cb)
            }));
        }

        test_utils::run_now(&session, move |_, object_cache| {
            let actions = unwrap!(object_cache.get_mdata_entry_actions(handle));
            assert_eq!(actions.len(), 3);

            match unwrap!(actions.get(&key0)) {
                &EntryAction::Ins(Value { ref content, entry_version: 0 }) if *content ==
                                                                              value0 => (),
                _ => panic!("Unexpected action"),
            }

            match unwrap!(actions.get(&key1)) {
                &EntryAction::Update(Value { ref content, entry_version }) if *content == value1 &&
                                                                              entry_version ==
                                                                              version1 => (),
                _ => panic!("Unexpected action"),
            }

            match unwrap!(actions.get(&key2)) {
                &EntryAction::Del(version) if version == version2 => (),
                _ => panic!("Unexpected action"),
            }
        });

        unsafe {
            unwrap!(test_utils::call_0(|ud, cb| mdata_entry_actions_free(&session, handle, ud, cb)))
        };

        test_utils::run_now(&session, move |_, object_cache| {
            assert!(object_cache.get_mdata_entry_actions(handle).is_err())
        });
    }
}
