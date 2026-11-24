use sp_core::crypto::{Ss58AddressFormat, Ss58Codec};
use eagle_runtime::AccountId;

/// Canonical SVY text form. Chain storage continues to use the 32-byte account ID.
pub(crate) fn decode(address: &str) -> Result<AccountId, String> {
    let encoded = address.strip_prefix("SVY").ok_or("expected an SVY address")?;
    let (account, format) = AccountId::from_ss58check_with_version(encoded)
        .map_err(|_| "invalid SVY address checksum or length")?;
    if format != Ss58AddressFormat::custom(42) || account.to_ss58check_with_version(format) != encoded {
        return Err("invalid SVY address encoding".into());
    }
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_address_requires_the_exact_prefix_network_and_checksum() {
        let account = AccountId::from([23_u8; 32]);
        let inner = account.to_ss58check_with_version(Ss58AddressFormat::custom(42));
        assert_eq!(decode(&format!("SVY{inner}")).unwrap(), account);
        for bad in [inner.clone(), format!("svy{inner}"), format!("SVY{inner}1"),
            format!("SVY{}", account.to_ss58check_with_version(Ss58AddressFormat::custom(0)))] {
            assert!(decode(&bad).is_err());
        }
    }
}
