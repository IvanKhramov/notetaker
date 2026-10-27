use anyhow::{Result, ensure};

/// Keep native thread pools bounded, including on hosts with SMT.
pub fn num_threads(requested: u32) -> Result<i32> {
    ensure!(requested <= 8, "num_threads must be 0 (auto) or 1..8");
    Ok(if requested == 0 {
        num_cpus::get_physical().clamp(1, 8) as i32
    } else {
        requested as i32
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_threads() {
        assert!((1..=8).contains(&num_threads(0).unwrap()));
        assert_eq!(num_threads(1).unwrap(), 1);
        assert_eq!(num_threads(8).unwrap(), 8);
        assert!(num_threads(9).is_err());
    }
}
