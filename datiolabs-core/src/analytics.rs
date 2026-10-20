use crate::models::Venta;
use rust_decimal::Decimal;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MetricasPeriodo {
    pub total_usd: Decimal,
    pub total_bs: Decimal,
    pub tickets: usize,
    pub ticket_promedio_usd: Decimal,
}

pub struct AnalyticsEngine;

impl Default for AnalyticsEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalyticsEngine {
    pub fn new() -> Self {
        Self
    }

    /// Calcula metricas de negocio agregadas sobre un conjunto de ventas del periodo.
    pub fn calcular_metricas(&self, ventas: &[Venta]) -> MetricasPeriodo {
        let mut total_usd = Decimal::ZERO;
        let mut total_bs = Decimal::ZERO;
        let tickets = ventas.len();

        for v in ventas {
            total_usd += v.total_usd;
            total_bs += v.total_bs;
        }

        let ticket_promedio_usd = if tickets > 0 {
            total_usd / Decimal::from(tickets)
        } else {
            Decimal::ZERO
        };

        MetricasPeriodo {
            total_usd,
            total_bs,
            tickets,
            ticket_promedio_usd: ticket_promedio_usd.round_dp(2),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{EstadoVenta, LineasVenta};
    use rust_decimal_macros::dec;

    #[test]
    fn test_metricas_comerciales() {
        let engine = AnalyticsEngine::new();
        let ventas = vec![
            Venta {
                id: "V1".into(),
                etiqueta: "".into(),
                es_cuenta_abierta: false,
                estado: EstadoVenta::Cerrada,
                lineas: LineasVenta::nuevas(),
                tasa_del_dia: dec!(36.00),
                total_usd: dec!(100.00),
                total_bs: dec!(3600.00),
                monto_recibido_bs: dec!(3600.00),
                vuelto_bs: dec!(0.00),
                pagos: Vec::new(),
                estado_vuelto: None,
                metodo_vuelto: None,
                monto_vuelto_usd: None,
                tasa_vuelto: None,
                fecha_apertura_unix: 1000,
                fecha_cierre_unix: 1000,
                firma_sha256: "".into(),
                tipo: "venta".into(),
                cliente: None,
                nota: None,
                abonos_usd: None,
                abonos_bs: None,
            },
            Venta {
                id: "V2".into(),
                etiqueta: "".into(),
                es_cuenta_abierta: false,
                estado: EstadoVenta::Cerrada,
                lineas: LineasVenta::nuevas(),
                tasa_del_dia: dec!(36.00),
                total_usd: dec!(50.00),
                total_bs: dec!(1800.00),
                monto_recibido_bs: dec!(1800.00),
                vuelto_bs: dec!(0.00),
                pagos: Vec::new(),
                estado_vuelto: None,
                metodo_vuelto: None,
                monto_vuelto_usd: None,
                tasa_vuelto: None,
                fecha_apertura_unix: 2000,
                fecha_cierre_unix: 2000,
                firma_sha256: "".into(),
                tipo: "venta".into(),
                cliente: None,
                nota: None,
                abonos_usd: None,
                abonos_bs: None,
            },
        ];

        let metricas = engine.calcular_metricas(&ventas);
        assert_eq!(metricas.total_usd, dec!(150.00));
        assert_eq!(metricas.total_bs, dec!(5400.00));
        assert_eq!(metricas.tickets, 2);
        assert_eq!(metricas.ticket_promedio_usd, dec!(75.00));
    }
}
