//! Torn state (the hardware-crash window): a movement without its stock update, or the
//! reverse, must still reload and leave the app operable.
use datiolabs_core::{
    capacidades::CAP_UNITARIA,
    db::Database,
    models::{MotivoMovimiento, MovimientoStock, Nombre, Producto, Sku},
};
use rust_decimal_macros::dec;

fn base(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("datiolabs-torn-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn seed(dir: &std::path::Path) {
    let db = Database::abrir(dir.to_str().unwrap()).unwrap();
    let prod = Producto::nuevo_simple(
        Sku::new("TORN1").unwrap(),
        Nombre::new("Torn").unwrap(),
        dec!(1),
        dec!(0),
        dec!(1000),
        CAP_UNITARIA,
    );
    db.guardar_producto(&prod).unwrap();
}

fn mov(id: &str, delta: rust_decimal::Decimal, unix: i64) -> MovimientoStock {
    MovimientoStock {
        id: id.into(),
        sku: "TORN1".into(),
        delta,
        motivo: MotivoMovimiento::Ajuste,
        venta_id: None,
        fecha_unix: unix,
        firma_sha256: String::new(),
    }
}

#[test]
fn torn_movement_only_reloads_and_continues() {
    let dir = base("movonly");
    seed(&dir);

    // Movement persisted, stock update lost.
    let db = Database::abrir(dir.to_str().unwrap()).unwrap();
    db.inner_db()
        .open_tree("movimientos")
        .unwrap()
        .insert(
            "k-torn",
            bincode::serialize(&mov("m-torn", dec!(-1), 1)).unwrap(),
        )
        .unwrap();
    db.inner_db().flush().unwrap();

    let cat = db.cargar_catalogo().expect("catalogo recarga sin error");
    let idx = cat.indice_de("TORN1").expect("SKU presente en el tree map");
    assert_eq!(cat.stock(idx), dec!(1000));

    db.aplicar_movimiento(mov("m2", dec!(-1), 2), |_, _| {})
        .expect("sistema sigue operable");
    let cat2 = db.cargar_catalogo().unwrap();
    assert_eq!(cat2.stock(cat2.indice_de("TORN1").unwrap()), dec!(999));
}

#[test]
fn torn_product_only_reloads_and_continues() {
    let dir = base("prodownly");
    seed(&dir);

    // Stock updated, movement lost.
    let db = Database::abrir(dir.to_str().unwrap()).unwrap();
    {
        let tree = db.inner_db().open_tree("productos").unwrap();
        let bytes = tree.get("TORN1").unwrap().expect("producto");
        let mut prod: Producto = bincode::deserialize(&bytes).unwrap();
        prod.stock = dec!(999);
        tree.insert("TORN1", bincode::serialize(&prod).unwrap())
            .unwrap();
    }
    db.inner_db().flush().unwrap();

    let cat = db.cargar_catalogo().expect("catalogo recarga sin error");
    let idx = cat.indice_de("TORN1").expect("SKU presente en el tree map");
    assert_eq!(cat.stock(idx), dec!(999));

    db.aplicar_movimiento(mov("m3", dec!(-1), 3), |_, _| {})
        .expect("sistema sigue operable");
    let cat2 = db.cargar_catalogo().unwrap();
    assert_eq!(cat2.stock(cat2.indice_de("TORN1").unwrap()), dec!(998));
}
