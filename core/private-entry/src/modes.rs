/// The mode of a file only its owner may read and write.
pub const OWNER_ONLY_FILE_MODE: u32 = 0o600;
/// The mode of a directory only its owner may read, write and search.
pub const OWNER_ONLY_DIRECTORY_MODE: u32 = 0o700;
/// The bits of a file's mode that are permissions.
pub const PERMISSION_BITS_MASK: u32 = 0o777;
pub const GROUP_AND_OTHER_PERMISSION_BITS: u32 = 0o077;
