use std::{ffi::c_void, io, ptr::null_mut, slice};

use anyhow::{Context, Result};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, LocalFree, ERROR_INSUFFICIENT_BUFFER, HANDLE},
    Security::{
        Authorization::ConvertSidToStringSidW, GetLengthSid, GetTokenInformation, IsValidSid,
        TokenUser, PSID, TOKEN_QUERY, TOKEN_USER,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

pub fn current_windows_user_sid() -> Result<String> {
    CurrentProcessTokenUser::current()
        .and_then(|identity| identity.sid_string())
        .context("query current Windows process-token user SID")
}

pub(crate) struct CurrentProcessTokenUser {
    _token: TokenHandle,
    token_user: AlignedTokenInformation,
}

impl CurrentProcessTokenUser {
    pub(crate) fn current() -> io::Result<Self> {
        // Daemon supervision and named-pipe ownership are both identities of
        // this process, so deliberately query its primary token rather than a
        // potentially impersonated thread token.
        let mut token = null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = TokenHandle(token);

        let mut required = 0;
        let first =
            unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &raw mut required) };
        let first_error = unsafe { GetLastError() };
        let required = token_user_information_size(first, first_error, required)?;
        if required < std::mem::size_of::<TOKEN_USER>() {
            return Err(invalid_token_user("TokenUser buffer is too small"));
        }
        let mut token_user = AlignedTokenInformation::new(required)?;
        let mut returned = u32::try_from(required)
            .map_err(|_| invalid_token_user("TokenUser size exceeds the Win32 API limit"))?;
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                token_user.as_mut_ptr().cast(),
                returned,
                &raw mut returned,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let returned = usize::try_from(returned)
            .map_err(|_| invalid_token_user("returned TokenUser size does not fit in memory"))?;
        if returned < std::mem::size_of::<TOKEN_USER>() || returned > token_user.byte_len() {
            return Err(invalid_token_user(
                "GetTokenInformation returned an invalid TokenUser size",
            ));
        }
        token_user.set_initialized_len(returned);

        let identity = Self {
            _token: token,
            token_user,
        };
        let _ = identity.sid_size()?;
        Ok(identity)
    }

    pub(crate) fn sid(&self) -> PSID {
        unsafe { (*self.token_user.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }

    fn sid_size(&self) -> io::Result<usize> {
        const SID_HEADER_BYTES: usize = 8;

        let sid = self.sid();
        if sid.is_null()
            || !self.token_user.contains_range(sid, SID_HEADER_BYTES)
            || unsafe { IsValidSid(sid) } == 0
        {
            return Err(invalid_token_user("process TokenUser SID is invalid"));
        }
        let sid_len = unsafe { GetLengthSid(sid) } as usize;
        if sid_len == 0 || !self.token_user.contains_range(sid, sid_len) {
            return Err(invalid_token_user(
                "process TokenUser SID falls outside its information buffer",
            ));
        }
        Ok(sid_len)
    }

    fn sid_string(&self) -> io::Result<String> {
        let _ = self.sid_size()?;
        let sid_string = LocalSidString::from_sid(self.sid())?;
        sid_string.to_string()
    }
}

fn token_user_information_size(
    query_result: i32,
    query_error: u32,
    required: u32,
) -> io::Result<usize> {
    if query_result != 0 {
        return Err(invalid_token_user(
            "TokenUser size query unexpectedly succeeded",
        ));
    }
    if query_error != ERROR_INSUFFICIENT_BUFFER {
        return Err(io::Error::from_raw_os_error(
            i32::try_from(query_error).unwrap_or(i32::MAX),
        ));
    }
    if required == 0 {
        return Err(invalid_token_user(
            "TokenUser size query returned no required length",
        ));
    }
    usize::try_from(required)
        .map_err(|_| invalid_token_user("TokenUser size does not fit in memory"))
}

struct TokenHandle(HANDLE);

impl Drop for TokenHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

struct AlignedTokenInformation {
    words: Vec<usize>,
    initialized_len: usize,
}

impl AlignedTokenInformation {
    fn new(byte_len: usize) -> io::Result<Self> {
        let word = std::mem::size_of::<usize>();
        let words = byte_len
            .checked_add(word - 1)
            .ok_or_else(|| invalid_token_user("TokenUser allocation size overflow"))?
            / word;
        Ok(Self {
            words: vec![0; words],
            initialized_len: 0,
        })
    }

    fn byte_len(&self) -> usize {
        self.words.len() * std::mem::size_of::<usize>()
    }

    fn set_initialized_len(&mut self, initialized_len: usize) {
        self.initialized_len = initialized_len;
    }

    fn as_ptr(&self) -> *const usize {
        self.words.as_ptr()
    }

    fn as_mut_ptr(&mut self) -> *mut usize {
        self.words.as_mut_ptr()
    }

    fn contains_range(&self, pointer: *const c_void, len: usize) -> bool {
        let start = self.as_ptr().addr();
        let Some(end) = start.checked_add(self.initialized_len) else {
            return false;
        };
        let pointer = pointer.addr();
        pointer >= start
            && pointer
                .checked_add(len)
                .is_some_and(|pointer_end| pointer_end <= end)
    }
}

struct LocalSidString(*mut u16);

impl LocalSidString {
    fn from_sid(sid: PSID) -> io::Result<Self> {
        let mut value = null_mut();
        if unsafe { ConvertSidToStringSidW(sid, &raw mut value) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if value.is_null() {
            return Err(invalid_token_user(
                "ConvertSidToStringSidW returned a null string",
            ));
        }
        Ok(Self(value))
    }

    fn to_string(&self) -> io::Result<String> {
        let mut len = 0;
        while unsafe { *self.0.add(len) } != 0 {
            len += 1;
        }
        String::from_utf16(unsafe { slice::from_raw_parts(self.0, len) })
            .map_err(|_| invalid_token_user("converted SID is not valid UTF-16"))
    }
}

impl Drop for LocalSidString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = LocalFree(self.0.cast());
            }
        }
    }
}

fn invalid_token_user(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;

    #[test]
    fn token_user_size_query_preserves_unexpected_win32_error_before_zero_length() {
        let error = token_user_information_size(0, ERROR_ACCESS_DENIED, 0)
            .expect_err("unexpected Win32 error must be preserved");

        assert_eq!(error.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
    }

    #[test]
    fn token_user_size_query_rejects_zero_length_after_expected_win32_error() {
        let error = token_user_information_size(0, ERROR_INSUFFICIENT_BUFFER, 0)
            .expect_err("zero TokenUser length must be rejected");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.raw_os_error(), None);
    }

    #[test]
    fn current_process_token_user_has_a_canonical_sid_string() -> Result<()> {
        let sid = current_windows_user_sid()?;
        assert!(sid.starts_with("S-1-"), "{sid}");
        assert!(sid.bytes().all(|byte| byte.is_ascii_graphic()), "{sid}");
        Ok(())
    }
}
