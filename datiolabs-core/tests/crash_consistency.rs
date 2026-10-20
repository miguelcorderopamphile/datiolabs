//! Crash-consistency: kill the worker (SIGKILL) while it applies movements, then check that
//! stock always matches the persisted movements.
use datiolabs_core::{
    capacidades::CAP_UNITARIA,
    db::Database,
    models::{Nombre, Producto, Sku},
};
use rust_decimal_macros::dec;
use std::time::Duration;

fn abrir(dir: &std::path::Path) -> Database {
    let mut last = None;
    for _ in 0..100 {
        match Database::abrir(dir.to_str().unwrap()) {
            Ok(db) => return db,
            Err(e) => {
                last = Some(e);
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    panic!("could not open {dir:?}: {last:?}");
}

fn seed(dir: &std::path::Path) {
    let db = abrir(dir);
    let prod = Producto::nuevo_simple(
        Sku::new("CRASH1").unwrap(),
        Nombre::new("Crash").unwrap(),
        dec!(1),
        dec!(0),
        dec!(1000),
        CAP_UNITARIA,
    );
    db.guardar_producto(&prod).unwrap();
}

#[test]
fn crash_kill_keeps_movement_stock_consistent() {
    let base = std::env::temp_dir().join("datiolabs-crash");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let mut inconsistencias = 0u32;
    for iter in 0..25u64 {
        let dir = base.join(format!("db{iter}"));
        std::fs::create_dir_all(&dir).unwrap();
        seed(&dir);

        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_crash_worker"))
            .arg(dir.to_str().unwrap())
            .spawn()
            .expect("spawn worker");
        std::thread::sleep(Duration::from_millis(2 + (iter * 7) % 30));
        child.kill().expect("kill -9");
        let _ = child.wait();

        // One opener, like the app; read the movement tree through it.
        let db = abrir(&dir);
        let n = db.inner_db().open_tree("movimientos").unwrap().len();
        let cat = db.cargar_catalogo().expect("catalog");
        let idx = cat.indice_de("CRASH1").expect("sku");
        let stock = cat.stock(idx);
        let esperado = dec!(1000) - rust_decimal::Decimal::from(n as u64);
        if stock != esperado {
            inconsistencias += 1;
            eprintln!("iter {iter}: stock={stock} movimientos={n} esperado={esperado}");
        }
    }
    assert_eq!(inconsistencias, 0, "inconsistencias movimiento↔stock");
}
