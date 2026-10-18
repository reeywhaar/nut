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