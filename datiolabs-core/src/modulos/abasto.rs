use crate::capacidades::{CAP_PESABLE, CAP_UNITARIA};

/// Mascara de capacidades permitidas para un producto de abasto.
pub const fn capacidades_permitidas() -> u16 {
    CAP_UNITARIA | CAP_PESABLE
}
