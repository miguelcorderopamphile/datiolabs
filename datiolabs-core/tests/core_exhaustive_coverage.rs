use datiolabs_core::capacidades::{
    CAP_UNITARIA, CLAVES_UNIVERSALES, RUBRO_ABASTO, es_licencia_universal_autorizada,
    generar_licencia_slot, validar_licencia_universal,
};
use datiolabs_core::db::Database;
use datiolabs_core::models::{
    Catalogo, ConfigNegocio, EventoTasaBcv, Nombre, Presentacion, Producto, Sku,
};
use datiolabs_core::modulos::retail::{
    AtributoVariante, ComisionVendedor, EstadoGarantia, Garantia, Variante,
};
use rust_decimal_macros::dec;

#[test]
fn test_capacidades_licencia_canonica() {
    assert_eq!(CLAVES_UNIVERSALES.len(), 10);
    for (slot, clave_esperada) in CLAVES_UNIVERSALES.iter().enumerate() {
        let clave = generar_licencia_slot(slot);
        assert_eq!(clave.len(), 16);
        assert_eq!(clave, *clave_esperada);
        assert!(
            es_licencia_universal_autorizada(&clave),
            "La licencia canonica en slot {} debe ser autorizada",
            slot
        );
        assert!(
            validar_licencia_universal(&clave),
            "La licencia canonica en slot {} debe validar",
            slot
        );
    }

    // La clave del cliente (ranura 1) es perpetua y valida.
    assert!(validar_licencia_universal("0000000000000000"));
    // Claves fuera de la lista no pasan, aunque tengan la forma de 16 digitos.
    assert!(!validar_licencia_universal("1662373670170269"));
    assert!(!validar_licencia_universal("1234567890123456"));
}

#[test]
fn test_db_semilla_inner_db_tasas_y_config() {
    let dir = std::env::temp_dir().join(format!("datio_test_cov_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ledger = Database::abrir(dir.to_str().unwrap()).expect("abrir database");

    // Verificar inner_db
    let inner = ledger.inner_db();
    assert!(!inner.was_recovered());

    // Verificar que semilla_si_vacia inserto las tasas por defecto
    let tasas_def = ledger.listar_tasas_impuestos().expect("tasas impuestos");
    assert_eq!(tasas_def.len(), 3);
    assert_eq!(tasas_def[0].id, "IVA-0");

    // Insertar tasas cronologicas y consultar ultimas_tasas
    let mut f1 = [0u8; 32];
    let bcv_bytes = b"BCV";
    f1[..bcv_bytes.len()].copy_from_slice(bcv_bytes);

    let tasa1 = EventoTasaBcv {
        valor_bs_por_usd: dec!(800.00),
        fecha_unix: 1726000000,
        fuente: f1,
        fuente_len: bcv_bytes.len() as u8,
        firma_sha256: String::new(),
    };
    let tasa2 = EventoTasaBcv {
        valor_bs_por_usd: dec!(810.50),
        fecha_unix: 1726000100,
        fuente: f1,
        fuente_len: bcv_bytes.len() as u8,
        firma_sha256: String::new(),
    };

    let t1_res = ledger.insertar_tasa(tasa1).expect("insertar tasa1");
    assert!(!t1_res.firma_sha256.is_empty());
    let t2_res = ledger.insertar_tasa(tasa2).expect("insertar tasa2");
    assert!(!t2_res.firma_sha256.is_empty());

    let ultimas = ledger.ultimas_tasas(5).expect("ultimas tasas");
    assert_eq!(ultimas.len(), 2);
    // Ultima debe ser tasa2 por orden descendente
    assert_eq!(ultimas[0].valor_bs_por_usd, dec!(810.50));
    assert_eq!(ultimas[1].valor_bs_por_usd, dec!(800.00));

    // Config: actualizar_config
    let mut conf = ConfigNegocio {
        nombre: Nombre::new("Bodega Test").unwrap(),
        rubros: RUBRO_ABASTO,
        pin_dueno_sha256: "hash123".into(),
        licencia_clave: String::new(),
        licencia_titular: String::new(),
        licencia_estado: String::new(),
        privacidad_inventario: false,
    };

    // Intentar actualizar sin haber guardado -> Error
    assert!(ledger.actualizar_config(&conf).is_err());

    // Guardar inicial
    ledger.guardar_config(&conf).expect("guardar config");

    // Actualizar existente
    conf.nombre = Nombre::new("Bodega Modificada").unwrap();
    ledger.actualizar_config(&conf).expect("actualizar config");

    let cargada = ledger.cargar_config().expect("cargar").expect("existe");
    assert_eq!(cargada.nombre.as_str(), "Bodega Modificada");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_db_actualizar_producto_y_catalogo_transform() {
    let dir = std::env::temp_dir().join(format!("datio_test_prod_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ledger = Database::abrir(dir.to_str().unwrap()).expect("abrir database");

    let mut prod = Producto::nuevo_simple(
        Sku::new("PROD-TEST").unwrap(),
        Nombre::new("Producto Original").unwrap(),
        dec!(5.00),
        dec!(16.00),
        dec!(50.00),
        CAP_UNITARIA,
    );
    prod.presentaciones.push(Presentacion {
        nombre: "Bulto 10".to_string(),
        precio_usd: dec!(45.00),
        unidades: 10,
    });

    ledger.guardar_producto(&prod).expect("guardar");

    // Actualizar in place
    prod.nombre = Nombre::new("Producto Modificado").unwrap();
    prod.precio_usd = dec!(6.50);
    ledger.actualizar_producto(&prod).expect("actualizar");

    let cat = ledger.cargar_catalogo().expect("cargar catalogo");
    assert_eq!(cat.len(), 1);
    assert_eq!(cat.nombre(0), "Producto Modificado");
    assert_eq!(cat.precio_usd(0), dec!(6.50));

    // Validar metodos SoA a_producto y desde_productos
    let prods_vec = vec![prod.clone()];
    let cat2 = Catalogo::desde_productos(prods_vec);
    assert_eq!(cat2.len(), 1);
    assert_eq!(cat2.sku_obj(0), prod.sku);
    let p_reconstruido = cat2.a_producto(0);
    assert_eq!(p_reconstruido.sku, prod.sku);
    assert_eq!(p_reconstruido.nombre, prod.nombre);
    assert_eq!(p_reconstruido.precio_usd, prod.precio_usd);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_db_auto_backup_rotacion_y_purgado() {
    let dir = std::env::temp_dir().join(format!("datio_test_bkp_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ledger = Database::abrir(dir.to_str().unwrap()).expect("abrir database");

    let bkp_dir = dir.join("backups");
    std::fs::create_dir_all(&bkp_dir).unwrap();
    let bkp_dir_str = bkp_dir.to_str().unwrap();

    // Crear respaldos automaticos con limite 2
    for _ in 0..3 {
        let _ = ledger.auto_backup(bkp_dir_str, 2).expect("auto_backup");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let lista = ledger.listar_backups(bkp_dir_str).expect("listar");
    assert!(lista.len() <= 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_anulaciones_libro_persistencia_backup_y_sin_residuos() {
    use datiolabs_core::models::{Anulacion, EstadoVenta, LineasVenta, Venta};

    let dir = std::env::temp_dir().join(format!("datio_test_anu_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let db = Database::abrir(dir.to_str().unwrap()).expect("abrir database");

    let mut lineas = LineasVenta::nuevas();
    lineas.agregar(
        Sku::new("CERVEZA-POLAR-6P").unwrap(),
        Nombre::new("Cerveza Polar Six-Pack").unwrap(),
        dec!(1),
        dec!(4.50),
        dec!(800.50),
        "Six-Pack".to_string(),
    );
    let venta = Venta {
        id: "VEN-ANU-001".to_string(),
        etiqueta: String::new(),
        es_cuenta_abierta: false,
        estado: EstadoVenta::Cerrada,
        lineas,
        tasa_del_dia: dec!(800.50),
        total_usd: dec!(4.50),
        total_bs: dec!(3602.25),
        monto_recibido_bs: dec!(4000),
        vuelto_bs: dec!(397.75),
        pagos: Vec::new(),
        estado_vuelto: Some("PAGADO".to_string()),
        metodo_vuelto: Some("BS.EFEC.".to_string()),
        monto_vuelto_usd: None,
        tasa_vuelto: None,
        fecha_apertura_unix: 1790000000,
        fecha_cierre_unix: 1790000100,
        firma_sha256: String::new(),
        tipo: "venta".to_string(),
        cliente: Some("Cajera Uno".to_string()),
        nota: None,
        abonos_usd: None,
        abonos_bs: None,
    };

    db.guardar_anulacion(&Anulacion {
        id: "ANU-001".to_string(),
        venta: venta.clone(),
        fecha_anulacion_unix: 1790000200,
        jornada_id: Some("JOR-001".to_string()),
        jornada_origen_id: Some("JOR-001".to_string()),
        operador: Some("Cajera Uno".to_string()),
        motivo: Some("ERROR OPERATIVO".to_string()),
        firma_sha256: String::new(),
    })
    .expect("guardar anulacion");

    let lista = db.listar_anulaciones(10).expect("listar anulaciones");
    assert_eq!(lista.len(), 1);
    assert_eq!(lista[0].venta.id, "VEN-ANU-001");
    assert!(
        !lista[0].firma_sha256.is_empty(),
        "la anulacion debe quedar firmada"
    );

    // El respaldo incluye el libro y lo restaura.
    let bkp = dir.join("anu.backup");
    db.exportar_backup(bkp.to_str().unwrap()).expect("exportar");

    let dir2 = std::env::temp_dir().join(format!("datio_test_anu2_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir2);
    let db2 = Database::abrir(dir2.to_str().unwrap()).expect("abrir database 2");
    db2.importar_backup(bkp.to_str().unwrap(), "")
        .expect("importar");
    assert_eq!(db2.listar_anulaciones(10).expect("listar").len(), 1);

    // Un respaldo viejo (sin arbol de anulaciones) limpia el libro para no
    // dejar residuos de otra base.
    let fixture_dir = std::env::temp_dir().join(format!("datio_test_anu3_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&fixture_dir);
    let ruta_fixture = fixture_dir.join("viejo.backup");
    std::fs::create_dir_all(&fixture_dir).unwrap();
    // Respaldo "viejo" generado en el momento (sin datos en anulaciones): mismo
    // semantico que el fixture del cliente, sin arrastrar datos de cliente.
    {
        let db_viejo = Database::abrir(fixture_dir.join("db_viejo").to_str().unwrap())
            .expect("abrir database viejo");
        db_viejo
            .exportar_backup(ruta_fixture.to_str().unwrap())
            .expect("generar respaldo viejo");
    }
    let db3 = Database::abrir(fixture_dir.join("db").to_str().unwrap()).expect("abrir database 3");
    db3.guardar_anulacion(&Anulacion {
        id: "ANU-RESIDUO".to_string(),
        venta,
        fecha_anulacion_unix: 1790000300,
        jornada_id: None,
        jornada_origen_id: None,
        operador: None,
        motivo: None,
        firma_sha256: String::new(),
    })
    .expect("residuo");
    assert_eq!(db3.listar_anulaciones(10).expect("listar").len(), 1);
    db3.importar_backup(ruta_fixture.to_str().unwrap(), "0000000000000000")
        .expect("importar fixture viejo");
    assert_eq!(
        db3.listar_anulaciones(10).expect("listar").len(),
        0,
        "el respaldo viejo no debe dejar residuos en el libro de anulaciones"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
    let _ = std::fs::remove_dir_all(&fixture_dir);
}

#[test]
fn test_redondeos_persistencia_firma_y_backup() {
    use datiolabs_core::models::Redondeo;

    let dir = std::env::temp_dir().join(format!("datio_test_red_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let db = Database::abrir(dir.to_str().unwrap()).expect("abrir database");

    db.guardar_redondeo(&Redondeo {
        id: "RED-001".to_string(),
        venta_id: "VEN-0001".to_string(),
        monto_bs: dec!(100.00),
        monto_usd: Some(dec!(0.125)),
        tasa: dec!(800.00),
        fecha_unix: 1790000000,
        jornada_id: Some("JOR-001".to_string()),
        operador: Some("Cajera Uno".to_string()),
        metodo: Some("BS.EFEC.".to_string()),
        firma_sha256: String::new(),
    })
    .expect("guardar redondeo");

    let lista = db.listar_redondeos(10).expect("listar redondeos");
    assert_eq!(lista.len(), 1);
    assert_eq!(lista[0].monto_bs, dec!(100.00));
    assert!(
        !lista[0].firma_sha256.is_empty(),
        "el redondeo debe quedar firmado"
    );

    let bkp = dir.join("red.backup");
    db.exportar_backup(bkp.to_str().unwrap()).expect("exportar");
    let dir2 = std::env::temp_dir().join(format!("datio_test_red2_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir2);
    let db2 = Database::abrir(dir2.to_str().unwrap()).expect("abrir database 2");
    db2.importar_backup(bkp.to_str().unwrap(), "")
        .expect("importar");
    assert_eq!(db2.listar_redondeos(10).expect("listar").len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

#[test]
fn test_models_sku_nombre_empty_from_slice_serde() {
    let sku_empty = Sku::empty();
    assert_eq!(sku_empty.as_str(), "");
    assert_eq!(sku_empty.as_bytes().len(), 0);

    let sku_slice = Sku::from_slice(b"SKU-SLICE-123");
    assert_eq!(sku_slice.as_str(), "SKU-SLICE-123");

    // Serde Sku
    let json_sku = serde_json::to_string(&sku_slice).expect("serialize sku");
    let de_sku: Sku = serde_json::from_str(&json_sku).expect("deserialize sku");
    assert_eq!(sku_slice, de_sku);

    let nom_empty = Nombre::empty();
    assert_eq!(nom_empty.as_str(), "");

    let nom_slice = Nombre::from_slice(b"Arroz Blanco 1KG");
    assert_eq!(nom_slice.as_str(), "Arroz Blanco 1KG");

    // Serde Nombre
    let json_nom = serde_json::to_string(&nom_slice).expect("serialize nombre");
    let de_nom: Nombre = serde_json::from_str(&json_nom).expect("deserialize nombre");
    assert_eq!(nom_slice, de_nom);
}

#[test]
fn test_modulos_retail_serde_tipos_array() {
    // AtributoVariante
    let mut attr = AtributoVariante::default();
    let clv = b"COLOR";
    let val = b"AZUL";
    attr.clave[..clv.len()].copy_from_slice(clv);
    attr.clave_len = clv.len() as u8;
    attr.valor[..val.len()].copy_from_slice(val);
    attr.valor_len = val.len() as u8;

    let json_attr = serde_json::to_string(&attr).expect("ser attr");
    let de_attr: AtributoVariante = serde_json::from_str(&json_attr).expect("de attr");
    assert_eq!(de_attr.clave_len, clv.len() as u8);

    // Variante
    let mut attrs = [AtributoVariante::default(); 8];
    attrs[0] = attr;
    let var = Variante {
        sku: "VAR-001".to_string(),
        nombre: "Camisa Azul".to_string(),
        precio_usd: dec!(15.00),
        stock: dec!(20),
        atributos: attrs,
        num_atributos: 1,
    };
    let json_var = serde_json::to_string(&var).expect("ser var");
    let de_var: Variante = serde_json::from_str(&json_var).expect("de var");
    assert_eq!(de_var.sku, var.sku);

    // Garantia
    let mut serie_buf = [0u8; 64];
    let s_bytes = b"SN-998877";
    serie_buf[..s_bytes.len()].copy_from_slice(s_bytes);
    let gar = Garantia {
        sku: "PROD-TECH".to_string(),
        serie: serie_buf,
        serie_len: s_bytes.len() as u8,
        fecha_venta_unix: 1726000000,
        duracion_dias: 365,
        estado: EstadoGarantia::Vigente,
    };
    let json_gar = serde_json::to_string(&gar).expect("ser gar");
    let de_gar: Garantia = serde_json::from_str(&json_gar).expect("de gar");
    assert_eq!(de_gar.duracion_dias, 365);

    // ComisionVendedor
    let mut vend_buf = [0u8; 32];
    let v_bytes = b"VEND-01";
    vend_buf[..v_bytes.len()].copy_from_slice(v_bytes);
    let com = ComisionVendedor {
        vendedor_id: vend_buf,
        vendedor_len: v_bytes.len() as u8,
        porcentaje: dec!(5.00),
        monto_acumulado: dec!(120.50),
    };
    let json_com = serde_json::to_string(&com).expect("ser com");
    let de_com: ComisionVendedor = serde_json::from_str(&json_com).expect("de com");
    assert_eq!(de_com.monto_acumulado, dec!(120.50));
}
