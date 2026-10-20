//! Integridad del backup: round-trip export→import, y rechazo de backup tamper/truncado.
use datiolabs_core::{
    capacidades::CAP_UNITARIA,
    db::Database,
    models::{Nombre, Producto, Sku},
};
use rust_decimal_macros::dec;

fn seed(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    let db = Database::abrir(dir.to_str().unwrap()).unwrap();
    for i in 0..5u32 {
        let prod = Producto::nuevo_simple(
            Sku::new(&format!("SKU{i}")).unwrap(),
            Nombre::new(&format!("Prod {i}")).unwrap(),
            dec!(10),
            dec!(16),
            dec!(100),
            CAP_UNITARIA,
        );
        db.guardar_producto(&prod).unwrap();
    }
}

#[test]
fn backup_roundtrip_preserves_data() {
    let base = std::env::temp_dir().join("apex-backup");
    let src = base.join("src");
    seed(&src);
    let backup = base.join("snap.backup");
    let _ = std::fs::remove_file(&backup);
    {
        let db = Database::abrir(src.to_str().unwrap()).unwrap();
        let meta = db.exportar_backup(backup.to_str().unwrap()).unwrap();
        eprintln!(
            "backup records={} checksum={}",
            meta.total_registros,
            &meta.checksum_sha256[..12]
        );
    }
    let dst = base.join("dst");
    let _ = std::fs::remove_dir_all(&dst);
    std::fs::create_dir_all(&dst).unwrap();
    let db2 = Database::abrir(dst.to_str().unwrap()).unwrap();
    db2.importar_backup(backup.to_str().unwrap(), "")
        .expect("import");
    let cat = db2.cargar_catalogo().expect("catalog");
    assert_eq!(cat.len(), 5);
    for i in 0..5 {
        assert_eq!(cat.sku(i), &format!("SKU{i}"));
        assert_eq!(cat.stock(i), dec!(100));
    }
}

#[test]
fn tampered_backup_is_rejected() {
    let base = std::env::temp_dir().join("apex-backup-tamper");
    let src = base.join("src");
    seed(&src);
    let backup = base.join("snap.backup");
    {
        let db = Database::abrir(src.to_str().unwrap()).unwrap();
        db.exportar_backup(backup.to_str().unwrap()).unwrap();
    }
    let mut bytes = std::fs::read(&backup).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    std::fs::write(&backup, &bytes).unwrap();

    let dst = base.join("dst");
    let _ = std::fs::remove_dir_all(&dst);
    std::fs::create_dir_all(&dst).unwrap();
    let db2 = Database::abrir(dst.to_str().unwrap()).unwrap();
    let res = db2.importar_backup(backup.to_str().unwrap(), "");
    eprintln!("tamper -> {res:?}");
    assert!(res.is_err(), "un backup alterado fue ACEPTADO");
}

#[test]
fn truncated_backup_is_rejected() {
    let base = std::env::temp_dir().join("apex-backup-trunc");
    let src = base.join("src");
    seed(&src);
    let backup = base.join("snap.backup");
    {
        let db = Database::abrir(src.to_str().unwrap()).unwrap();
        db.exportar_backup(backup.to_str().unwrap()).unwrap();
    }
    let bytes = std::fs::read(&backup).unwrap();
    std::fs::write(&backup, &bytes[..bytes.len().saturating_sub(10)]).unwrap();

    let dst = base.join("dst");
    let _ = std::fs::remove_dir_all(&dst);
    std::fs::create_dir_all(&dst).unwrap();
    let db2 = Database::abrir(dst.to_str().unwrap()).unwrap();
    let res = db2.importar_backup(backup.to_str().unwrap(), "");
    eprintln!("trunc -> {res:?}");
    assert!(res.is_err(), "un backup truncado fue ACEPTADO");
}
