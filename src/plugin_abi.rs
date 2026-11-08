#![allow(unsafe_code)]
//! YooDesk native plugin ABI v1. Only C layouts, byte buffers and function pointers cross DLLs.
use std::ffi::{c_char, c_void};

pub const ABI_VERSION: u32 = 1;
pub const MAX_CONTROL_BYTES: usize = 1024 * 1024;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Buffer {
    pub data: *mut u8,
    pub len: usize,
    pub capacity: usize,
}

impl Buffer {
    pub const fn empty() -> Self {
        Self {
            data: std::ptr::null_mut(),
            len: 0,
            capacity: 0,
        }
    }
    pub fn owned(mut bytes: Vec<u8>) -> Self {
        let result = Self {
            data: bytes.as_mut_ptr(),
            len: bytes.len(),
            capacity: bytes.capacity(),
        };
        std::mem::forget(bytes);
        result
    }
    pub unsafe fn bytes(&self) -> Result<&[u8], &'static str> {
        if self.len > MAX_CONTROL_BYTES
            || self.capacity < self.len
            || (self.len != 0 && self.data.is_null())
        {
            return Err("invalid plugin control buffer");
        }
        if self.len == 0 {
            return Ok(&[]);
        }
        Ok(std::slice::from_raw_parts(self.data, self.len))
    }
}

pub type Call = unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut Buffer) -> i32;
pub type Free = unsafe extern "C" fn(Buffer);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct HostApi {
    pub abi_version: u32,
    pub struct_size: usize,
    pub context: *mut c_void,
    pub access: Call,
    pub free: Free,
}
// The host keeps the AccessAuthority alive until all plugin threads stop.
unsafe impl Send for HostApi {}
unsafe impl Sync for HostApi {}

#[repr(C)]
pub struct PluginApi {
    pub abi_version: u32,
    pub struct_size: usize,
    pub id: *const c_char,
    pub start: unsafe extern "C" fn(
        *const HostApi,
        *const u8,
        usize,
        *mut *mut c_void,
        *mut Buffer,
    ) -> i32,
    pub command: Call,
    pub stop: unsafe extern "C" fn(*mut c_void),
    pub free: Free,
    pub worker: unsafe extern "C" fn(*const u8, usize, *mut Buffer) -> i32,
}
unsafe impl Sync for PluginApi {}

pub unsafe extern "C" fn free(buffer: Buffer) {
    if !buffer.data.is_null() && buffer.capacity >= buffer.len {
        drop(Vec::from_raw_parts(
            buffer.data,
            buffer.len,
            buffer.capacity,
        ));
    }
}
