//! Worker para el test de crash-consistency: aplica movimientos en loop hasta ser kill -9.
use datiolabs_core::{
    db::Database,
    models::{MotivoMovimiento, MovimientoStock},
};
use rust_decimal_macros::dec;

fn main() {
    let path = std::env::args().nth(1).expect("db path");
    let db = Database::abrir(&path).expect("open db");
    loop {
        let mov = MovimientoStock {
            id: uuid::Uuid::new_v4().to_string(),
            sku: "CRASH1".to_string(),
            delta: dec!(-1),
            motivo: MotivoMovimiento::Ajuste,
            venta_id: None,
            fecha_unix: chrono::Utc::now().timestamp(),
            firma_sha256: String::new(),
        };
        let _ = db.aplicar_movimiento(mov, |_sku, _delta| {});
    }
}
