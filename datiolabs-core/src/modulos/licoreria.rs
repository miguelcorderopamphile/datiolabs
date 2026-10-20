use crate::capacidades::CAP_CUENTA_ABIERTA;
use crate::models::LineasVenta;
use rust_decimal::Decimal;

pub fn admite_cuenta_abierta(capacidades_negocio: u16) -> bool {
    capacidades_negocio & CAP_CUENTA_ABIERTA != 0
}

/// Liquidacion de cierre: cada linea conserva su tasa de consumo; el total
/// nunca se recalcula con la tasa del dia (directiva de tasa bloqueada).
#[derive(Debug, Clone, PartialEq)]
pub struct CierreCuenta {
    pub total_usd: Decimal,
    pub total_bs: Decimal,
    pub lineas: usize,
}

pub fn liquidar_cierre(lineas: &LineasVenta) -> CierreCuenta {
    CierreCuenta {
        total_usd: lineas.total_usd(),
        total_bs: lineas.total_bs(),
        lineas: lineas.skus.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Nombre, Sku};
    use rust_decimal_macros::dec;

    #[test]
    fn cierre_consolida_tasas_heterogeneas_por_linea() {
        let mut lineas = LineasVenta::nuevas();
        lineas.agregar(
            Sku::new("RON").unwrap(),
            Nombre::new("Ron").unwrap(),
            dec!(2),
            dec!(5),
            dec!(100),
            "unidad".to_string(),
        );
        lineas.agregar(
            Sku::new("RON").unwrap(),
            Nombre::new("Ron").unwrap(),
            dec!(1),
            dec!(6),
            dec!(110),
            "unidad".to_string(),
        );
        let cierre = liquidar_cierre(&lineas);
        assert_eq!(cierre.total_usd, dec!(16));
        assert_eq!(cierre.total_bs, dec!(1660));
        assert_eq!(cierre.lineas, 2);
    }

    #[test]
    fn cuenta_abierta_segun_capacidades_del_negocio() {
        assert!(admite_cuenta_abierta(CAP_CUENTA_ABIERTA));
        assert!(!admite_cuenta_abierta(0));
    }
}
