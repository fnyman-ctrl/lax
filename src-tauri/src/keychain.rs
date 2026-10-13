//! The database key, kept in the login keychain (ADR 0010).
//!
//! Lax will encrypt its database with a random 256-bit key that lives only
//! in the user's login keychain: a generic password of 64 hex characters
//! (service [`SERVICE`], account [`ACCOUNT`], label [`LABEL`]), never
//! synced. Hex, so the user can see and copy it in Keychain Access.
//!
//! The rules this module keeps:
//!
//! - **A new key only when there is nothing to lose.** [`key_for`] makes one
//!   only when the lookup found no key item *and* the database isn't
//!   encrypted ([`DatabaseFile`]). Every other failure (locked, denied or
//!   cancelled, anything unexpected) fails closed and makes nothing.
//! - **Never overwritten, never deleted.** [`KeySource::add_item`] only adds:
//!   if an item is already there it says so and leaves it alone. Nothing
//!   here updates or deletes a key item.
//! - **The key stays out of errors and logs.** [`KeyError`] holds a fixed
//!   kind and at most the Keychain's status code, and [`DatabaseKey`] has no
//!   `Display` and a `Debug` that hides it.
//! - **Lax's own copies are zeroed.** The key and its hex form are held in
//!   [`Zeroizing`] buffers. That clears only Lax's copies: the Keychain's
//!   result, the `CFData` handed to `SecItemAdd` and freed heap may still
//!   hold it (ADR 0010, "The key").
//!
//! The store asks a [`KeySource`], so tests use `fake::FakeKeychain` and never
//! touch the real Keychain, which could block on a prompt. The one test that
//! does is `#[ignore]`d; see `tests::real_login_keychain`.
//!
//! [`own_build_hash`] reads this build's code hash (cdhash). macOS asks once
//! per new build before it lets Lax read its key, and Lax compares this hash
//! with the last one to show its own note before that prompt.
//!
//! Nothing calls this module yet (ADR 0010, rollout step 1).

// Calls into the Security framework; each one has a SAFETY comment.
#![allow(unsafe_code)]

use std::fmt;
use std::ptr::{self, NonNull};

use objc2_core_foundation::{CFData, CFDictionary, CFRetained, CFString, CFType, kCFBooleanFalse};
use security_framework::passwords::{PasswordOptions, generic_password};
use security_framework::random::SecRandom;
use zeroize::Zeroizing;

/// The key item's service: Lax's bundle identifier.
pub const SERVICE: &str = "io.github.fnyman-ctrl.lax";
/// The key item's account.
pub const ACCOUNT: &str = "database-key";
/// The key item's name in Keychain Access.
pub const LABEL: &str = "Lax database key";

/// The key's length: 256 bits.
pub const KEY_BYTES: usize = 32;
/// The stored form's length: two hex characters per byte.
const HEX_CHARS: usize = KEY_BYTES * 2;

// Security framework status codes (`SecBase.h`, `MacErrors.h`).
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
const ERR_SEC_DUPLICATE_ITEM: i32 = -25299;
const ERR_SEC_USER_CANCELED: i32 = -128;
const ERR_SEC_AUTH_FAILED: i32 = -25293;
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;
const ERR_SEC_INTERACTION_REQUIRED: i32 = -25315;
const ERR_SEC_SUCCESS: i32 = 0;

/// Why Lax couldn't get its key. Every case fails closed: none of them is a
/// reason to make a new key except [`KeyError::Missing`], and only when
/// [`key_for`] is told the database isn't encrypted.
///
/// Holds no key material, only a kind and at most a status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    /// The keychain is locked and can't be unlocked without the user
    /// (`errSecInteractionNotAllowed`, `errSecInteractionRequired`). The key
    /// is probably fine: "Try again".
    Locked,
    /// The user denied Lax the key or cancelled the prompt
    /// (`errSecAuthFailed`, `errSecUserCanceled`). The key is probably fine:
    /// "Try again".
    Denied,
    /// There is no key item (`errSecItemNotFound`).
    Missing,
    /// Anything else: an unexpected Keychain status (kept, it isn't secret),
    /// or `None` for a key item that isn't a key Lax made, a key that didn't
    /// read back as it was stored, or no random bytes.
    Other(Option<i32>),
}

impl KeyError {
    /// Maps a Security framework status code to one of the cases above.
    /// An unknown code is never [`KeyError::Missing`].
    pub fn from_status(status: i32) -> Self {
        match status {
            ERR_SEC_ITEM_NOT_FOUND => KeyError::Missing,
            ERR_SEC_USER_CANCELED | ERR_SEC_AUTH_FAILED => KeyError::Denied,
            ERR_SEC_INTERACTION_NOT_ALLOWED | ERR_SEC_INTERACTION_REQUIRED => KeyError::Locked,
            other => KeyError::Other(Some(other)),
        }
    }
}

/// The database key. Never printed. Kept on the heap, so moving a
/// `DatabaseKey` moves only a pointer and leaves no copy of the key on the
/// stack, and zeroed there before the memory is freed.
pub struct DatabaseKey(Box<Zeroizing<[u8; KEY_BYTES]>>);

impl DatabaseKey {
    /// A new key: 32 bytes from `SecRandomCopyBytes`.
    fn random() -> Result<Self, KeyError> {
        let mut key = Box::new(Zeroizing::new([0u8; KEY_BYTES]));
        SecRandom::default()
            .copy_bytes(key.as_mut_slice())
            .map_err(|_| KeyError::Other(None))?;
        Ok(DatabaseKey(key))
    }

    /// The key's bytes, for SQLCipher.
    pub fn as_bytes(&self) -> &[u8; KEY_BYTES] {
        &self.0
    }

    /// The stored form: 64 lowercase hex characters, in a buffer that is
    /// zeroed when dropped. Allocated once at its full size, so no partial
    /// copy is left behind by a reallocation.
    pub fn to_hex(&self) -> Zeroizing<String> {
        let mut hex = Zeroizing::new(String::with_capacity(HEX_CHARS));
        for byte in self.0.iter() {
            hex.push(hex_digit(byte >> 4));
            hex.push(hex_digit(byte & 0x0f));
        }
        hex
    }

    /// Reads the stored form: exactly 64 hex characters, in either case.
    /// Spaces and line breaks around them are ignored, since a user who
    /// copied the key by hand into a new item may have pasted some. Anything
    /// else isn't a key Lax made.
    fn from_hex(hex: &[u8]) -> Option<Self> {
        let hex = hex.trim_ascii();
        if hex.len() != HEX_CHARS {
            return None;
        }
        let mut key = Box::new(Zeroizing::new([0u8; KEY_BYTES]));
        let (pairs, _) = hex.as_chunks::<2>();
        for (byte, [high, low]) in key.iter_mut().zip(pairs) {
            *byte = (hex_value(*high)? << 4) | hex_value(*low)?;
        }
        Some(DatabaseKey(key))
    }

    /// Whether two keys are the same, without stopping at the first
    /// difference.
    fn matches(&self, other: &DatabaseKey) -> bool {
        self.0
            .iter()
            .zip(other.0.iter())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
    }
}

impl fmt::Debug for DatabaseKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DatabaseKey(hidden)")
    }
}

fn hex_digit(nibble: u8) -> char {
    char::from(b"0123456789abcdef"[usize::from(nibble & 0x0f)])
}

fn hex_value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// What adding a key item did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Added {
    /// The item was added.
    New,
    /// An item was already there. It was left as it was.
    AlreadyThere,
}

/// Where the key item lives. The real one is [`LoginKeychain`]; tests use
/// `fake::FakeKeychain`.
pub trait KeySource {
    /// The key item's data, as stored. Never makes an item.
    fn read_item(&self) -> Result<Zeroizing<Vec<u8>>, KeyError>;

    /// Adds a key item holding `data`. Never replaces an existing item: if
    /// there is one, returns [`Added::AlreadyThere`] and leaves it alone.
    fn add_item(&self, data: &[u8]) -> Result<Added, KeyError>;
}

/// What `lax.sqlite` is when Lax asks for its key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseFile {
    /// Absent, empty or plaintext: nothing is locked to a key yet, so a
    /// missing key may be made.
    NotEncrypted,
    /// Encrypted: only its own key opens it, so a missing key is an error,
    /// never a reason to make a new one.
    Encrypted,
}

/// Reads the existing key. Never makes one.
pub fn read_key(source: &impl KeySource) -> Result<DatabaseKey, KeyError> {
    let item = source.read_item()?;
    DatabaseKey::from_hex(&item).ok_or(KeyError::Other(None))
}

/// The key for the database: the stored one, or, only if there is none and
/// `file` isn't encrypted, a new one, stored and read back first.
pub fn key_for(source: &impl KeySource, file: DatabaseFile) -> Result<DatabaseKey, KeyError> {
    match read_key(source) {
        Err(KeyError::Missing) if file == DatabaseFile::NotEncrypted => create_key(source),
        found => found,
    }
}

/// Makes a key, stores it and reads it back, so Lax never encrypts with a
/// key the Keychain doesn't hold (ADR 0010, "Converting", step 2).
///
/// Never returns [`KeyError::Missing`]: Lax has just tried to make the key,
/// so a missing one now is something else, and never a reason to try again
/// with another new key.
fn create_key(source: &impl KeySource) -> Result<DatabaseKey, KeyError> {
    let not_missing = |error| match error {
        KeyError::Missing => KeyError::Other(None),
        other => other,
    };
    let key = DatabaseKey::random()?;
    let added = source
        .add_item(key.to_hex().as_bytes())
        .map_err(not_missing)?;
    let stored = read_key(source).map_err(not_missing)?;
    match added {
        // Someone stored a key between the lookup and the add: use theirs,
        // never replace it.
        Added::AlreadyThere => Ok(stored),
        Added::New if stored.matches(&key) => Ok(stored),
        Added::New => Err(KeyError::Other(None)),
    }
}

/// The key item in the user's login keychain.
///
/// `SecItemAdd` puts it in the user's default keychain, which is the login
/// keychain unless the user has chosen another default in Keychain Access.
/// Reads search every keychain in the user's search list.
pub struct LoginKeychain {
    service: &'static str,
    account: &'static str,
    label: &'static str,
}

impl LoginKeychain {
    /// Lax's own key item.
    pub fn lax() -> Self {
        LoginKeychain {
            service: SERVICE,
            account: ACCOUNT,
            label: LABEL,
        }
    }
}

impl KeySource for LoginKeychain {
    fn read_item(&self) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        let mut options = PasswordOptions::new_generic_password(self.service, self.account);
        options.set_access_synchronized(Some(false));
        generic_password(options)
            .map(Zeroizing::new)
            .map_err(|error| KeyError::from_status(error.code()))
    }

    fn add_item(&self, data: &[u8]) -> Result<Added, KeyError> {
        // security-framework's `set_generic_password` updates an existing
        // item, which Lax must never do, and its `ItemAddOptions` can't say
        // "not synced". So the item is added with `SecItemAdd` directly.
        let (
            Some(class_key),
            Some(generic_class),
            Some(service_key),
            Some(account_key),
            Some(label_key),
            Some(synchronizable_key),
            Some(value_key),
            Some(no),
        ) = (
            kSecClass,
            kSecClassGenericPassword,
            kSecAttrService,
            kSecAttrAccount,
            kSecAttrLabel,
            kSecAttrSynchronizable,
            kSecValueData,
            // SAFETY: a constant CoreFoundation provides, never changed.
            unsafe { kCFBooleanFalse },
        )
        else {
            return Err(KeyError::Other(None));
        };
        let service = CFString::from_str(self.service);
        let account = CFString::from_str(self.account);
        let label = CFString::from_str(self.label);
        let value = CFData::from_bytes(data);
        let keys: [&CFString; 6] = [
            class_key,
            service_key,
            account_key,
            label_key,
            synchronizable_key,
            value_key,
        ];
        let values: [&CFType; 6] = [
            generic_class.as_ref(),
            service.as_ref(),
            account.as_ref(),
            label.as_ref(),
            no.as_ref(),
            value.as_ref(),
        ];
        let attributes = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
        // SAFETY: `attributes` is a valid dictionary for the call; a null
        // result pointer asks for no result, so nothing is handed back.
        let status = unsafe { SecItemAdd(attributes.as_opaque(), ptr::null_mut()) };
        match status {
            ERR_SEC_SUCCESS => Ok(Added::New),
            ERR_SEC_DUPLICATE_ITEM => Ok(Added::AlreadyThere),
            other => Err(KeyError::from_status(other)),
        }
    }
}

/// What identifies this build of Lax to the Keychain: its code hash
/// (cdhash). Not secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildHash(Vec<u8>);

impl BuildHash {
    /// Lowercase hex, for the `key-build` file.
    pub fn to_hex(&self) -> String {
        self.0
            .iter()
            .flat_map(|byte| [hex_digit(byte >> 4), hex_digit(byte & 0x0f)])
            .collect()
    }
}

/// This build's code hash, from `SecCodeCopySelf` and
/// `SecCodeCopySigningInformation` (`kSecCodeInfoUnique`). `None` if it
/// can't be read, for example for unsigned code, which then counts as a
/// new build.
pub fn own_build_hash() -> Option<BuildHash> {
    let unique_key = kSecCodeInfoUnique?;
    let mut code: *const CFType = ptr::null();
    // SAFETY: on success `code` holds a new code object the caller owns
    // ("Copy" rule); CFRetained releases it once.
    let status = unsafe { SecCodeCopySelf(DEFAULT_FLAGS, &mut code) };
    if status != ERR_SEC_SUCCESS {
        return None;
    }
    // SAFETY: as above.
    let code = unsafe { CFRetained::from_raw(NonNull::new(code.cast_mut())?) };
    let mut info: *const CFType = ptr::null();
    // SAFETY: `code` is a valid code object, which the function accepts in
    // place of a static one; on success `info` holds a new dictionary the
    // caller owns.
    let status = unsafe { SecCodeCopySigningInformation(&code, DEFAULT_FLAGS, &mut info) };
    if status != ERR_SEC_SUCCESS {
        return None;
    }
    // SAFETY: as above.
    let info = unsafe { CFRetained::from_raw(NonNull::new(info.cast_mut())?) };
    let info = info.downcast::<CFDictionary>().ok()?;
    // SAFETY: the signing information's keys are strings.
    let info = unsafe { CFRetained::cast_unchecked::<CFDictionary<CFString, CFType>>(info) };
    let unique = info.get(unique_key)?.downcast::<CFData>().ok()?;
    let bytes = unique.to_vec();
    (!bytes.is_empty()).then_some(BuildHash(bytes))
}

/// `kSecCSDefaultFlags`.
const DEFAULT_FLAGS: u32 = 0;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    // SAFETY: constant strings the Security framework provides and never
    // changes; `None` if one is missing.
    safe static kSecClass: Option<&'static CFString>;
    safe static kSecClassGenericPassword: Option<&'static CFString>;
    safe static kSecAttrService: Option<&'static CFString>;
    safe static kSecAttrAccount: Option<&'static CFString>;
    safe static kSecAttrLabel: Option<&'static CFString>;
    safe static kSecAttrSynchronizable: Option<&'static CFString>;
    safe static kSecValueData: Option<&'static CFString>;
    safe static kSecCodeInfoUnique: Option<&'static CFString>;

    // On success with a non-null `result`, a new reference the caller owns.
    fn SecItemAdd(attributes: &CFDictionary, result: *mut *const CFType) -> i32;
    // On success, `code` holds a new code object the caller owns.
    fn SecCodeCopySelf(flags: u32, code: *mut *const CFType) -> i32;
    // On success, `information` holds a new dictionary the caller owns.
    fn SecCodeCopySigningInformation(
        code: &CFType,
        flags: u32,
        information: *mut *const CFType,
    ) -> i32;
}

#[cfg(test)]
pub mod fake {
    //! A key source in memory, for tests. Never touches the Keychain.

    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    use zeroize::Zeroizing;

    use super::{Added, KeyError, KeySource};

    #[derive(Default)]
    pub struct FakeKeychain {
        /// The key item's data, or `None` for no item.
        pub item: RefCell<Option<Vec<u8>>>,
        /// Errors the next reads return, in order, before reads see `item`.
        pub read_errors: RefCell<VecDeque<KeyError>>,
        /// An error every add returns, without storing anything.
        pub add_error: Cell<Option<KeyError>>,
        /// What the keychain holds after an add instead of what was added.
        pub stored_instead: RefCell<Option<Vec<u8>>>,
        pub reads: Cell<usize>,
        pub adds: Cell<usize>,
    }

    impl FakeKeychain {
        pub fn holding(data: &[u8]) -> Self {
            let fake = FakeKeychain::default();
            *fake.item.borrow_mut() = Some(data.to_vec());
            fake
        }

        pub fn failing_reads(errors: &[KeyError]) -> Self {
            let fake = FakeKeychain::default();
            fake.read_errors.borrow_mut().extend(errors);
            fake
        }
    }

    impl KeySource for FakeKeychain {
        fn read_item(&self) -> Result<Zeroizing<Vec<u8>>, KeyError> {
            self.reads.set(self.reads.get() + 1);
            if let Some(error) = self.read_errors.borrow_mut().pop_front() {
                return Err(error);
            }
            match &*self.item.borrow() {
                Some(data) => Ok(Zeroizing::new(data.clone())),
                None => Err(KeyError::Missing),
            }
        }

        fn add_item(&self, data: &[u8]) -> Result<Added, KeyError> {
            self.adds.set(self.adds.get() + 1);
            if let Some(error) = self.add_error.get() {
                return Err(error);
            }
            let mut item = self.item.borrow_mut();
            if item.is_some() {
                return Ok(Added::AlreadyThere);
            }
            *item = Some(
                self.stored_instead
                    .borrow_mut()
                    .take()
                    .unwrap_or_else(|| data.to_vec()),
            );
            Ok(Added::New)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeKeychain;
    use super::*;

    const STORED: &[u8] = b"00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    fn stored_key() -> DatabaseKey {
        DatabaseKey::from_hex(STORED).unwrap()
    }

    // Status codes.

    #[test]
    fn a_missing_item_is_missing() {
        assert_eq!(KeyError::from_status(-25300), KeyError::Missing);
    }

    #[test]
    fn a_locked_keychain_is_locked() {
        // errSecInteractionNotAllowed, errSecInteractionRequired
        assert_eq!(KeyError::from_status(-25308), KeyError::Locked);
        assert_eq!(KeyError::from_status(-25315), KeyError::Locked);
    }

    #[test]
    fn a_denied_or_cancelled_prompt_is_denied() {
        // errSecAuthFailed, errSecUserCanceled
        assert_eq!(KeyError::from_status(-25293), KeyError::Denied);
        assert_eq!(KeyError::from_status(-128), KeyError::Denied);
    }

    #[test]
    fn anything_else_is_other_and_never_missing() {
        // errSecNotAvailable, errSecNoSuchKeychain, errSecNoDefaultKeychain,
        // errSecMissingEntitlement, errSecParam, errSecDuplicateItem, and
        // codes Lax has never seen.
        for status in [-25291, -25294, -25307, -34018, -50, -25299, -1, 1, i32::MIN] {
            assert_eq!(KeyError::from_status(status), KeyError::Other(Some(status)));
        }
    }

    // Reading an existing key.

    #[test]
    fn an_existing_key_is_read_and_no_key_is_made() {
        for file in [DatabaseFile::NotEncrypted, DatabaseFile::Encrypted] {
            let keychain = FakeKeychain::holding(STORED);
            let key = key_for(&keychain, file).unwrap();
            assert!(key.matches(&stored_key()));
            assert_eq!(keychain.adds.get(), 0);
            assert_eq!(keychain.item.borrow().as_deref(), Some(STORED));
        }
    }

    #[test]
    fn upper_case_hex_is_the_same_key() {
        let upper = STORED.to_ascii_uppercase();
        let keychain = FakeKeychain::holding(&upper);
        assert!(read_key(&keychain).unwrap().matches(&stored_key()));
    }

    #[test]
    fn spaces_and_line_breaks_around_a_pasted_key_are_ignored() {
        for pasted in [
            [STORED, b"\n"].concat(),
            [b" ", STORED, b" "].concat(),
            [b"\t", STORED, b"\r\n"].concat(),
        ] {
            let keychain = FakeKeychain::holding(&pasted);
            assert!(read_key(&keychain).unwrap().matches(&stored_key()));
        }
    }

    // Failing closed.

    #[test]
    fn locked_denied_and_other_fail_closed_and_make_no_key() {
        for error in [
            KeyError::Locked,
            KeyError::Denied,
            KeyError::Other(Some(-25291)),
        ] {
            for file in [DatabaseFile::NotEncrypted, DatabaseFile::Encrypted] {
                let keychain = FakeKeychain::failing_reads(&[error]);
                assert_eq!(key_for(&keychain, file).unwrap_err(), error);
                assert_eq!(keychain.adds.get(), 0, "{error:?} made a key");
                assert!(keychain.item.borrow().is_none());
            }
        }
    }

    #[test]
    fn a_missing_key_next_to_an_encrypted_database_makes_no_key() {
        let keychain = FakeKeychain::default();
        assert_eq!(
            key_for(&keychain, DatabaseFile::Encrypted).unwrap_err(),
            KeyError::Missing
        );
        assert_eq!(keychain.adds.get(), 0);
        assert!(keychain.item.borrow().is_none());
    }

    #[test]
    fn an_item_that_is_not_a_key_fails_closed_and_is_left_alone() {
        let not_keys: [&[u8]; 6] = [
            b"",
            &STORED[..63],
            b"00112233445566778899aabbccddeeff00112233445566778899aabbccddeef\n",
            b"00112233445566778899aabbccddeeff00112233445566778899aabbccddeeffaa",
            b"g0112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
            b"x'00112233445566778899aabbccddeeff00112233445566778899aabbccddee'",
        ];
        for data in not_keys {
            for file in [DatabaseFile::NotEncrypted, DatabaseFile::Encrypted] {
                let keychain = FakeKeychain::holding(data);
                assert_eq!(key_for(&keychain, file).unwrap_err(), KeyError::Other(None));
                assert_eq!(keychain.adds.get(), 0);
                assert_eq!(keychain.item.borrow().as_deref(), Some(data));
            }
        }
    }

    // Making a key once.

    #[test]
    fn a_key_is_made_once_then_read() {
        let keychain = FakeKeychain::default();
        let made = key_for(&keychain, DatabaseFile::NotEncrypted).unwrap();
        assert_eq!(keychain.adds.get(), 1);
        let stored = keychain.item.borrow().clone().unwrap();
        assert_eq!(stored.len(), 64);
        assert!(
            stored
                .iter()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
        );
        assert_eq!(stored.as_slice(), made.to_hex().as_bytes());

        for file in [DatabaseFile::NotEncrypted, DatabaseFile::Encrypted] {
            let again = key_for(&keychain, file).unwrap();
            assert!(again.matches(&made));
        }
        assert_eq!(keychain.adds.get(), 1);
        assert_eq!(keychain.item.borrow().as_deref(), Some(stored.as_slice()));
    }

    #[test]
    fn each_new_key_is_random() {
        let first = key_for(&FakeKeychain::default(), DatabaseFile::NotEncrypted).unwrap();
        let second = key_for(&FakeKeychain::default(), DatabaseFile::NotEncrypted).unwrap();
        assert!(!first.matches(&second));
        assert_ne!(first.as_bytes(), &[0u8; KEY_BYTES]);
    }

    #[test]
    fn a_key_stored_meanwhile_is_used_and_never_replaced() {
        // The lookup finds nothing, then someone stores a key before Lax adds.
        let keychain = FakeKeychain::failing_reads(&[KeyError::Missing]);
        *keychain.item.borrow_mut() = Some(STORED.to_vec());
        let key = key_for(&keychain, DatabaseFile::NotEncrypted).unwrap();
        assert!(key.matches(&stored_key()));
        assert_eq!(keychain.adds.get(), 1);
        assert_eq!(keychain.item.borrow().as_deref(), Some(STORED));
    }

    #[test]
    fn a_key_that_cannot_be_stored_is_not_used() {
        for error in [
            KeyError::Locked,
            KeyError::Denied,
            KeyError::Other(Some(-34018)),
        ] {
            let keychain = FakeKeychain::default();
            keychain.add_error.set(Some(error));
            assert_eq!(
                key_for(&keychain, DatabaseFile::NotEncrypted).unwrap_err(),
                error
            );
        }
        // Missing while adding: never a reason to try again with a new key.
        let keychain = FakeKeychain::default();
        keychain.add_error.set(Some(KeyError::Missing));
        assert_eq!(
            key_for(&keychain, DatabaseFile::NotEncrypted).unwrap_err(),
            KeyError::Other(None)
        );
    }

    #[test]
    fn a_key_that_does_not_read_back_is_not_used() {
        // Read back refused.
        for error in [KeyError::Locked, KeyError::Denied] {
            let keychain = FakeKeychain::failing_reads(&[KeyError::Missing, error]);
            assert_eq!(
                key_for(&keychain, DatabaseFile::NotEncrypted).unwrap_err(),
                error
            );
        }
        // Read back missing: never a reason to try again with a new key.
        let keychain = FakeKeychain::failing_reads(&[KeyError::Missing, KeyError::Missing]);
        assert_eq!(
            key_for(&keychain, DatabaseFile::NotEncrypted).unwrap_err(),
            KeyError::Other(None)
        );
        // Read back different.
        let keychain = FakeKeychain::default();
        *keychain.stored_instead.borrow_mut() = Some(STORED.to_vec());
        assert_eq!(
            key_for(&keychain, DatabaseFile::NotEncrypted).unwrap_err(),
            KeyError::Other(None)
        );
        assert_eq!(keychain.adds.get(), 1);
    }

    // The key in memory.

    #[test]
    fn the_key_round_trips_through_hex() {
        let key = stored_key();
        assert_eq!(key.to_hex().as_bytes(), STORED);
        assert_eq!(key.as_bytes()[..4], [0x00, 0x11, 0x22, 0x33]);
    }

    #[test]
    fn the_key_never_shows_in_debug_output() {
        let key = stored_key();
        let shown = format!("{key:?}");
        assert!(!shown.contains("0011"));
        assert!(!shown.contains("ddeeff"));
        assert_eq!(shown, "DatabaseKey(hidden)");
    }

    // This build's code hash. Reads only this process's own signature,
    // never the Keychain.

    // On Apple silicon the linker signs every binary (ad hoc), test binaries
    // too; on Intel they may be unsigned, and then there is no hash to read.
    #[cfg(target_arch = "aarch64")]
    #[test]
    fn this_build_has_a_code_hash() {
        let hash = own_build_hash().expect("no code hash");
        assert_eq!(hash.0.len(), 20, "cdhash is 20 bytes");
        assert_eq!(hash.to_hex().len(), 40);
        assert_eq!(own_build_hash(), Some(hash));
    }

    /// The real login keychain, with a test item of its own; Lax's key is
    /// never touched. Ignored by default: it writes to the login keychain
    /// and macOS may show a prompt. Run on a Mac with:
    ///
    /// `cargo test --locked keychain::tests::real_login_keychain -- --ignored`
    ///
    /// No prompt is expected, since the same build writes and reads (ADR
    /// 0010, spike result 1). The test item ("Lax keychain test") is deleted
    /// at the start and when the test ends, also when it fails.
    #[test]
    #[ignore = "writes to the real login keychain"]
    fn real_login_keychain() {
        const TEST_SERVICE: &str = "io.github.fnyman-ctrl.lax.test";
        const TEST_ACCOUNT: &str = "keychain-test";
        struct DeleteTestItem;
        impl Drop for DeleteTestItem {
            fn drop(&mut self) {
                // Only the test's own item; errors (none there) are fine.
                let _ = security_framework::passwords::delete_generic_password(
                    TEST_SERVICE,
                    TEST_ACCOUNT,
                );
            }
        }
        drop(DeleteTestItem);
        let _cleanup = DeleteTestItem;
        let keychain = LoginKeychain {
            service: TEST_SERVICE,
            account: TEST_ACCOUNT,
            label: "Lax keychain test",
        };

        assert_eq!(
            key_for(&keychain, DatabaseFile::Encrypted).unwrap_err(),
            KeyError::Missing
        );
        let made = key_for(&keychain, DatabaseFile::NotEncrypted).unwrap();
        let read = key_for(&keychain, DatabaseFile::Encrypted).unwrap();
        assert!(read.matches(&made));
        // A second add leaves the first key as it was.
        let other = DatabaseKey::random().unwrap();
        assert_eq!(
            keychain.add_item(other.to_hex().as_bytes()),
            Ok(Added::AlreadyThere)
        );
        assert!(read_key(&keychain).unwrap().matches(&made));
    }
}
