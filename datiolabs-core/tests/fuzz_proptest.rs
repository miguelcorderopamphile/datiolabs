use datiolabs_core::{
    capacidades::{
        CAP_COMISION, CAP_CUENTA_ABIERTA, CAP_GARANTIA, CAP_PERECEDERO, CAP_PESABLE, CAP_SERIE,
        CAP_UNITARIA, CAP_VARIANTES, ErrorNegocio, RUBRO_ABASTO, RUBRO_LICORERIA, RUBRO_PANADERIA,
        RUBRO_RETAIL, capacidades_de_rubros, rubros_activos, validar_linea, validar_vigencia,
    },
    db::Database,
    models::{
        Catalogo, ConfigNegocio, LineasVenta, MotivoMovimiento, MovimientoStock, Nombre, Producto,
        Sku,
    },
    modulos::panaderia::{LibroLotes, Lote},
    modulos::retail::{
        ComisionVendedor, calcular_comision, validar_atributo, validar_serie, validar_vendedor,
    },
};
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::TestRunner;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashSet;

fn arb_decimal() -> impl Strategy<Value = Decimal> {
    (-1000000i64..1000000i64).prop_map(|v| Decimal::from(v) / dec!(100))
}

fn arb_positive_decimal() -> impl Strategy<Value = Decimal> {
    (1i64..1000000i64).prop_map(|v| Decimal::from(v) / dec!(100))
}

fn arb_sku() -> impl Strategy<Value = String> {
    "[A-Z0-9_-]{1,32}".prop_map(|s: String| s.to_uppercase())
}

fn arb_rubros() -> impl Strategy<Value = u16> {
    (0u16..16u16).prop_filter("al menos un rubro", |r| *r != 0)
}

proptest! {
    // ──────────────────────────────────────────────
    // PROP: Rubros y capacidades - invariantes
    // ──────────────────────────────────────────────
    #[test]
    fn prop_rubros_capacidades_deduplicacion(rubros in arb_rubros()) {
        let caps = capacidades_de_rubros(rubros);

        // Unitaria siempre presente
        prop_assert!(caps & CAP_UNITARIA != 0);

        if rubros & RUBRO_ABASTO != 0 {
            prop_assert!(caps & CAP_PESABLE != 0);
        }
        if rubros & RUBRO_PANADERIA != 0 {
            prop_assert!(caps & CAP_PESABLE != 0);
            prop_assert!(caps & CAP_PERECEDERO != 0);
        }
        if rubros & RUBRO_LICORERIA != 0 {
            prop_assert!(caps & CAP_CUENTA_ABIERTA != 0);
        }
        if rubros & RUBRO_RETAIL != 0 {
            prop_assert!(caps & CAP_SERIE != 0);
            prop_assert!(caps & CAP_VARIANTES != 0);
            prop_assert!(caps & CAP_GARANTIA != 0);
            prop_assert!(caps & CAP_COMISION != 0);
        }

        let validos = rubros_activos(rubros);
        prop_assert_eq!(validos, (rubros & 0xF) != 0);
    }

    // ──────────────────────────────────────────────
    // PROP: Validación línea de venta
    // ──────────────────────────────────────────────
    #[test]
    fn prop_validar_linea_invariantes(
        caps in arb_rubros(),
        cantidad in arb_decimal(),
        stock in arb_decimal(),
    ) {
        let caps = capacidades_de_rubros(caps);
        let result = validar_linea(caps, cantidad, stock);

        if cantidad <= Decimal::ZERO {
            prop_assert_eq!(result, Err(ErrorNegocio::CantidadNoUnitaria(cantidad)));
        } else {
            let es_unitaria = caps & CAP_PESABLE == 0;
            if es_unitaria && cantidad.fract() != Decimal::ZERO {
                prop_assert_eq!(result, Err(ErrorNegocio::CantidadNoUnitaria(cantidad)));
            } else {
                if stock < cantidad {
                    prop_assert_eq!(result, Err(ErrorNegocio::StockInsuficiente { disponible: stock, solicitado: cantidad }));
                } else {
                    prop_assert!(result.is_ok());
                }
            }
        }
    }

    // ──────────────────────────────────────────────
    // PROP: Vigencia lote FEFO
    // ──────────────────────────────────────────────
    #[test]
    fn prop_validar_vigencia(caduc in 0i64..2_000_000_000i64, ahora in 0i64..2_000_000_000i64) {
        let result = validar_vigencia(caduc, ahora);

        if ahora >= caduc {
            prop_assert_eq!(result, Err(ErrorNegocio::LoteVencido { caduco_unix: caduc }));
        } else {
            prop_assert!(result.is_ok());
        }
    }

    // ──────────────────────────────────────────────
    // PROP: DB roundtrip - config único
    // ──────────────────────────────────────────────
    #[test]
    fn prop_db_config_unico(rubros in arb_rubros()) {
        let dir = std::env::temp_dir().join(format!("prop_db_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&dir);

        let db = Database::abrir(dir.to_str().unwrap()).unwrap();

        let config = ConfigNegocio::nuevo_simple(
            Nombre::new("Test").unwrap(),
            rubros,
            "pin123".into(),
        );
        prop_assert!(db.guardar_config(&config).is_ok());

        let config2 = ConfigNegocio::nuevo_simple(
            Nombre::new("Test2").unwrap(),
            RUBRO_ABASTO,
            "pin456".into(),
        );
        prop_assert!(matches!(
            db.guardar_config(&config2),
            Err(datiolabs_core::db::DbError::Negocio(ErrorNegocio::YaInicializado))
        ));

        let loaded = db.cargar_config().unwrap().unwrap();
        prop_assert_eq!(loaded.nombre.as_str(), "Test");
        prop_assert_eq!(loaded.rubros, rubros);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ──────────────────────────────────────────────
    // PROP: DB catálogo - SKU único, stock atómico
    // ──────────────────────────────────────────────
    #[test]
    fn prop_db_catalogo_stock_atomico(
        rubros in arb_rubros(),
        skus in proptest::collection::vec(arb_sku(), 1..10),
        precios in proptest::collection::vec(arb_positive_decimal(), 1..10),
        stocks in proptest::collection::vec(arb_decimal(), 1..10),
    ) {
        let dir = std::env::temp_dir().join(format!("prop_cat_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&dir);

        let db = Database::abrir(dir.to_str().unwrap()).unwrap();
        let _ = db.guardar_config(&ConfigNegocio::nuevo_simple(
            Nombre::new("Test").unwrap(),
            rubros,
            "".into(),
        ));

        let caps = capacidades_de_rubros(rubros);
        let mut inserted = HashSet::new();

        for ((sku_str, precio), stock) in skus.iter().zip(precios.iter()).zip(stocks.iter()) {
            if inserted.insert(sku_str.clone()) {
                let sku = Sku::new(sku_str).unwrap();
                let prod = Producto::nuevo_simple(
                    sku,
                    Nombre::new("Test").unwrap(),
                    *precio,
                    dec!(16),
                    (*stock).max(Decimal::ZERO),
                    caps,
                );
                prop_assert!(db.guardar_producto(&prod).is_ok());
            } else {
                let prod = Producto::nuevo_simple(
                    Sku::new(sku_str).unwrap(),
                    Nombre::new("Test").unwrap(),
                    *precio,
                    dec!(16),
                    Decimal::ZERO,
                    caps,
                );
                prop_assert!(matches!(
                    db.guardar_producto(&prod),
                    Err(datiolabs_core::db::DbError::Negocio(ErrorNegocio::SkuDuplicado(_)))
                ));
            }
        }

        let catalogo = db.cargar_catalogo().unwrap();
        prop_assert_eq!(catalogo.len(), inserted.len());

        for sku in &inserted {
            let initial_stock = catalogo.stock(catalogo.indice_de(sku).unwrap());

            let mov = MovimientoStock {
                id: uuid::Uuid::new_v4().to_string(),
                sku: sku.clone(),
                delta: dec!(-1),
                motivo: MotivoMovimiento::Venta,
                venta_id: Some(uuid::Uuid::new_v4().to_string()),
                fecha_unix: 1000,
                firma_sha256: String::new(),
            };

            let mut cat = catalogo.clone();
            let idx = cat.indice_de(sku).unwrap();
            let result = db.aplicar_movimiento(mov, |s, d| {
                if let Some(i) = cat.indice_de(s) {
                    cat.aplicar_delta_stock(i, d);
                }
            });

            let reloaded = db.cargar_catalogo().unwrap();
            let new_stock = reloaded.stock(reloaded.indice_de(sku).unwrap());
            if initial_stock >= dec!(1) {
                prop_assert!(result.is_ok());
                prop_assert_eq!(new_stock, initial_stock - dec!(1));
                prop_assert_eq!(cat.stock(idx), new_stock);
            } else {
                // Rigor: no se permite stock negativo ni se escribe el movimiento.
                let es_error_de_stock = matches!(
                    result,
                    Err(datiolabs_core::db::DbError::Negocio(
                        ErrorNegocio::StockInsuficiente { .. }
                    ))
                );
                prop_assert!(es_error_de_stock);
                prop_assert_eq!(new_stock, initial_stock);
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ──────────────────────────────────────────────
    // PROP: Panadería - FEFO multi-lote
    // ──────────────────────────────────────────────
    #[test]
    fn prop_panaderia_fefo_multi_lote(
        rubros in arb_rubros().prop_filter("panaderia", |r| *r & RUBRO_PANADERIA != 0),
        lotes in proptest::collection::vec(
            (arb_sku(), arb_positive_decimal(), 1i64..86400i64), 1..5
        ),
        consumo in arb_positive_decimal(),
    ) {
        let total_stock: Decimal = lotes.iter().map(|(_, c, _)| *c).sum();
        prop_assume!(consumo <= total_stock);
        let dir = std::env::temp_dir().join(format!("prop_pan_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&dir);

        let db = Database::abrir(dir.to_str().unwrap()).unwrap();
        let caps = capacidades_de_rubros(rubros);

        let _ = db.guardar_config(&ConfigNegocio::nuevo_simple(
            Nombre::new("Test").unwrap(),
            rubros,
            "".into(),
        ));

        let sku_str = "PANTEST";
        let sku = Sku::new(sku_str).unwrap();
        let prod = Producto::nuevo_simple(
            sku,
            Nombre::new("Pan Test").unwrap(),
            dec!(2),
            dec!(16),
            Decimal::ZERO,
            caps,
        );
        db.guardar_producto(&prod).unwrap();

        let mut libro = LibroLotes::nuevo();
        let ahora = 1_000_000i64;

        for (i, (_sku, cant, caduc_offset)) in lotes.iter().enumerate() {
            let lote = Lote {
                id: format!("L{}", i),
                sku: sku_str.into(),
                horneado_unix: ahora - 3600,
                cantidad_inicial: *cant,
                disponible: *cant,
                caduce_unix: ahora + caduc_offset,
            };
            libro.insertar(lote);
        }

        let consumido = libro.descontar_fefo(sku_str, consumo, ahora).unwrap();

        for (lote_id, cant) in consumido.iter() {
            let lote_par = libro.par_disponible_por_id(lote_id);
            prop_assert!(lote_par.is_some());
            prop_assert!(*cant > Decimal::ZERO);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ──────────────────────────────────────────────
    // PROP: Cuentas abiertas - tasa bloqueada por línea
    // ──────────────────────────────────────────────
    #[test]
    fn prop_cuenta_abierta_tasa_bloqueada(
        rubros in arb_rubros().prop_filter("licoreria", |r| *r & RUBRO_LICORERIA != 0),
        lineas in proptest::collection::vec(
            (arb_sku(), arb_positive_decimal(), arb_positive_decimal()), 1..5
        ),
    ) {
        let caps = capacidades_de_rubros(rubros);
        prop_assert!(caps & CAP_CUENTA_ABIERTA != 0);

        let mut lineas_venta = LineasVenta::nuevas();
        let mut total_bs_esperado = Decimal::ZERO;

        for (sku_str, precio_usd, tasa) in &lineas {
            let sku = Sku::new(sku_str).unwrap();
            let nombre = Nombre::new("Test").unwrap();
            lineas_venta.agregar(sku, nombre, dec!(1), *precio_usd, *tasa, "unidad".to_string());
            total_bs_esperado += *precio_usd * *tasa;
        }

        prop_assert_eq!(lineas_venta.total_bs(), total_bs_esperado);

        let mut runner = TestRunner::default();
        let tasa_dia = arb_positive_decimal().new_tree(&mut runner).unwrap().current();
        let mut lineas_con_tasa_dia = LineasVenta::nuevas();
        for (sku_str, precio_usd, _tasa) in &lineas {
            let sku = Sku::new(sku_str).unwrap();
            let nombre = Nombre::new("Test").unwrap();
            lineas_con_tasa_dia.agregar(sku, nombre, dec!(1), *precio_usd, tasa_dia, "unidad".to_string());
        }

        prop_assert_ne!(lineas_venta.total_bs(), lineas_con_tasa_dia.total_bs());
    }

    // ──────────────────────────────────────────────
    // PROP: Backup/Restore integridad
    // ──────────────────────────────────────────────
    #[test]
    fn prop_backup_restore_integridad(
        rubros in arb_rubros(),
        n_productos in 1..10usize,
    ) {
        let dir = std::env::temp_dir().join(format!("prop_bk_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&dir);

        let db = Database::abrir(dir.to_str().unwrap()).unwrap();
        let _ = db.guardar_config(&ConfigNegocio::nuevo_simple(
            Nombre::new("Test").unwrap(),
            rubros,
            "".into(),
        ));

        let caps = capacidades_de_rubros(rubros);
        let mut skus = HashSet::new();

        for i in 0..n_productos {
            let sku_str = format!("SKU{}", i);
            let sku = Sku::new(&sku_str).unwrap();
            let prod = Producto::nuevo_simple(
                sku,
                Nombre::new(&format!("Prod {}", i)).unwrap(),
                dec!(10) + Decimal::from(i),
                dec!(16),
                dec!(100) + Decimal::from(i * 10),
                caps,
            );
            db.guardar_producto(&prod).unwrap();
            skus.insert(sku_str);
        }

        for sku in &skus {
            let mov = MovimientoStock {
                id: format!("M{}", sku),
                sku: sku.clone(),
                delta: dec!(-5),
                motivo: MotivoMovimiento::Venta,
                venta_id: Some(format!("V{}", sku)),
                fecha_unix: 2000,
                firma_sha256: String::new(),
            };
            let _ = db.aplicar_movimiento(mov, |_, _| {});
        }

        let backup_path = dir.join("backup.test");
        let meta1 = db.exportar_backup(backup_path.to_str().unwrap()).unwrap();

        let dir2 = std::env::temp_dir().join(format!("prop_bk2_{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&dir2);
        let db2 = Database::abrir(dir2.to_str().unwrap()).unwrap();

        let meta2 = db2.importar_backup(backup_path.to_str().unwrap(), "").unwrap();

        prop_assert_eq!(meta1.version, meta2.version);
        prop_assert_eq!(meta1.total_registros, meta2.total_registros);
        prop_assert_eq!(meta1.checksum_sha256, meta2.checksum_sha256);
        prop_assert_eq!(meta1.arboles, meta2.arboles);

        let cat1 = db.cargar_catalogo().unwrap();
        let cat2 = db2.cargar_catalogo().unwrap();
        prop_assert_eq!(cat1.len(), cat2.len());

        for sku in &skus {
            let idx1 = cat1.indice_de(sku).unwrap();
            let idx2 = cat2.indice_de(sku).unwrap();
            prop_assert_eq!(cat1.stock(idx1), cat2.stock(idx2));
            prop_assert_eq!(cat1.precio_usd(idx1), cat2.precio_usd(idx2));
        }

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    // ──────────────────────────────────────────────
    // PROP: Retail - validaciones
    // ──────────────────────────────────────────────
    #[test]
    fn prop_retail_validaciones(
        serie in "[A-Za-z0-9\\-]{0,100}",
        clave in "[A-Za-z0-9]{0,50}",
        valor in "[A-Za-z0-9 ]{0,100}",
        vendedor in "[A-Za-z0-9]{0,50}",
    ) {
        let res_serie = validar_serie(&serie);
        if serie.is_empty() || serie.len() > 64 {
            prop_assert!(res_serie.is_err());
        } else {
            prop_assert!(res_serie.is_ok());
        }

        let res_attr = validar_atributo(&clave, &valor);
        if clave.is_empty() || clave.len() > 32 || valor.is_empty() || valor.len() > 64 {
            prop_assert!(res_attr.is_err());
        } else {
            prop_assert!(res_attr.is_ok());
        }

        let res_vend = validar_vendedor(&vendedor);
        if vendedor.is_empty() || vendedor.len() > 32 {
            prop_assert!(res_vend.is_err());
        } else {
            prop_assert!(res_vend.is_ok());
        }
    }

    // ──────────────────────────────────────────────
    // PROP: Retail - cálculo comisión
    // ──────────────────────────────────────────────
    #[test]
    fn prop_retail_comision(
        porcentaje in (0i64..10000i64).prop_map(|v| Decimal::from(v) / dec!(100)),
        monto in (1i64..1000000i64).prop_map(|v| Decimal::from(v) / dec!(100)),
    ) {
        let comision = ComisionVendedor {
            vendedor_id: [0u8; 32],
            vendedor_len: 4,
            porcentaje,
            monto_acumulado: dec!(0),
        };

        let calc = calcular_comision(&comision, monto);
        let esperado = (monto * porcentaje / dec!(100)).round_dp(2);

        prop_assert_eq!(calc, esperado);
    }

    // ──────────────────────────────────────────────
    // PROP: Catalogo SoA - valor inventario consistente
    // ──────────────────────────────────────────────
    #[test]
    fn prop_catalogo_valor_inventario(
        productos in proptest::collection::vec(
            (arb_sku(), arb_positive_decimal(), arb_decimal().prop_filter("stock>=0", |s| *s >= Decimal::ZERO)),
            1..20
        ),
    ) {
        let mut catalogo = Catalogo::nuevo();
        let mut expected = Decimal::ZERO;

        for (sku_str, precio, stock) in productos {
            if catalogo.indice_de(&sku_str).is_none() {
                let sku = Sku::new(&sku_str).unwrap();
                let prod = Producto::nuevo_simple(
                    sku,
                    Nombre::new("Test").unwrap(),
                    precio,
                    dec!(16),
                    stock,
                    CAP_UNITARIA,
                );
                catalogo.insertar(prod).unwrap();
                expected += precio * stock;
            }
        }

        prop_assert_eq!(catalogo.valor_inventario_usd(), expected);
    }
}
