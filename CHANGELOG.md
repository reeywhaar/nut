# 0.1.6
- Added `try_` variants that return errors instead of panicking: `Bucket::try_get`, `Bucket::try_bucket`, `Bucket::try_bucket_mut`, `Bucket::try_buckets`, `Bucket::try_stats`, `Tx::try_buckets`, `DB::try_stats` and `DB::try_info`. The panicking functions are deprecated
- Replaced panics with errors throughout the library and the `nut` CLI
- Corrupted files now produce errors instead of out-of-bounds reads
- Databases are no longer limited to 256 MiB, the maximum mmap size now matches Bolt
- Fixed lost updates when committing large transactions
- Fixed panics and tree corruption when rebalancing after deletes
- Fixed memory unsafety when a transaction opens many child buckets
- Fixed quadratic time and memory when committing large transactions
- Fixed reading and writing freelists of 65535 pages or more
- Fixed `DB::batch` reporting success when a handler failed, and a race between adding and running batch calls
- Fixed a panic when deleting a missing bucket that sorts after the last key
- Fixed `Info::data` to point at the mmap
- Transactions are rolled back when commit fails or when dropped during a panic
- Faster reads and writes

# 0.1.5
- Merged https://github.com/Reeywhaar/nut/pull/12 from [@aleksanderkrauze](https://github.com/aleksanderkrauze) `Replace ansi_term with anstyle and gate CLI dependencies behind feature flag`
- CLI dependencies are now behind the default `cli` feature, use `default-features = false` to depend on the library only
- Migrated to edition 2024, minimum supported Rust version is now 1.89
- Replaced `fs2` with std file locking and `fs4`, removed `memoffset`
- Updated dependencies
- Fixed clippy warnings and modernized code

# 0.1.4
- Merged https://github.com/Reeywhaar/nut/pull/11 from [@postrov](https://github.com/postrov) `Fix cursor prev() descending range`
- Linted cargo warnings

# 0.1.3
- Fix "nut pages" printing overflow and freed pages

# 0.1.2
- Minor fixes and housekeeping

# 0.1.1
- Minor fixes

# 0.1.0
- Initial release