# quotactl-rs

Low-level Rust wrapper around the Linux `quotactl(2)` syscall.
Provides device-path based APIs for managing disk quotas.

## What it provides

- Thin, typed Rust APIs for common quota operations (on/off, get/set, sync)
- Support for User/Group/Project quota types (filesystem-dependent)
- Optional XFS-specific operations where supported by the kernel/filesystem

## Examples

### General quota operations

```rust
use quotactl_rs::quota::{self, DqBlk, QuotaValid};
use quotactl_rs::QuotaType;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let device = Path::new("/dev/sda1");
    let id = 1000;

    // Read current quota
    let dqblk = quota::get_quota(QuotaType::User, device, id)?;
    println!("Current space: {} bytes", dqblk.dqb_curspace);

    // Set limits
    let new_limits = DqBlk {
        dqb_bhardlimit: 10240, // filesystem units (see quotactl docs)
        dqb_bsoftlimit: 8192,
        dqb_valid: QuotaValid::BLIMITS.bits(),
        ..Default::default()
    };

    quota::set_quota(QuotaType::User, device, id, new_limits)?;
    Ok(())
}
```

### XFS-specific operations

```rust
use quotactl_rs::{xfs_quota, QuotaType};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let device = Path::new("/dev/sdb1");

    let stat = xfs_quota::x_get_qstat(QuotaType::Project, device)?;
    println!("Quota flags: {:?}", stat.flags);

    xfs_quota::x_quota_on(QuotaType::Project, device)?;
    Ok(())
}
```

## Requirements

- Linux with quota support enabled
- Sufficient privileges for quota operations (often root / CAP_SYS_ADMIN)
- Filesystem support for the quota type you use (e.g., ext4 project quotas)

## License

Apache-2.0. See [LICENSE](../LICENSE).

Copyright 2025-2026 Hyundai Mobis Co., Ltd.
