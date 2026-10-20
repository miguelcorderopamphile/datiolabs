#![no_main]

use libfuzzer_sys::fuzz_target;
use datiolabs_core::{
    capacidades::{ErrorNegocio, RUBRO_ABASTO, RUBRO_PANADERIA, RUBRO_LICORERIA, RUBRO_RETAIL, capacidades_de_rubros, rubros_activos, validar_linea, validar_vigencia, CAP_UNITARIA, CAP_PESABLE, CAP_CUENTA_ABIERTA, CAP_PERECEDERO, CAP_SERIE, CAP_VARIANTES, CAP_GARANTIA, CAP_COMISION},
    db::Database,
    models::{Producto, ConfigNegocio, EstadoVenta, LineasVenta, Venta, MovimientoStock, MotivoMovimiento, Catalogo, Sku, Nombre},
    modulos::panaderia::{LibroLotes, Lote},
    modulos::retail::{Variante, AtributoVariante, Garantia, ComisionVendedor, EstadoGarantia, validar_serie, validar_atributo, validar_vendedor, calcular_comision},
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashSet;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    let mut cursor = 0;

    macro_rules! next_u8 {
        () => {{
            let v = data.get(cursor).copied().unwrap_or(0);
            cursor = (cursor + 1) % data.len();
            v
        }};
    }

    macro_rules! next_u16 {
        () => {{
            let b1 = next_u8!() as u16;
            let b2 = next_u8!() as u16;
            u16::from_le_bytes([b1, b2])
        }};
    }

    macro_rules! next_u64 {
        () => {{
            let mut bytes = [0u8; 8];
            for i in 0..8 {
                bytes[i] = next_u8!();
            }
            u64::from_le_bytes(bytes)
        }};
    }

    macro_rules! next_decimal {
        () => {{
            let raw = next_u64!();
            let sign = if (raw & 1) != 0 { -1 } else { 1 };
            let magnitude = (raw >> 1) % 10_000_000;
            Decimal::from(i64::from(magnitude) * sign) / dec!(100)
        }};
    }

    macro_rules! next_bool {
        () => {{
            next_u8!() % 2 == 0
        }};
    }

    macro_rules! next_string {
        ($max:expr) => {{
            let len = (next_u8!() as usize) % $max + 1;
            let mut s = String::with_capacity(len);
            for _ in 0..len {
                let c = next_u8!();
                if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' {
                    s.push(c as char);
                } else {
                    s.push('X');
                }
            }
            s
        }};
    }

    let rubros = next_u16!() & 0x0F;
    let caps = capacidades_de_rubros(rubros);
    let _ = rubros_activos(rubros);

    if rubros & RUBRO_ABASTO != 0 {
        assert!(caps & CAP_PESABLE != 0);
    }
    if rubros & RUBRO_PANADERIA != 0 {
        assert!(caps & CAP_PESABLE != 0);
        assert!(caps & CAP_PERECEDERO != 0);
    }
    if rubros & RUBRO_LICORERIA != 0 {
        assert!(caps & CAP_CUENTA_ABIERTA != 0);
    }
    if rubros & RUBRO_RETAIL != 0 {
        assert!(caps & CAP_SERIE != 0);
        assert!(caps & CAP_VARIANTES != 0);
        assert!(caps & CAP_GARANTIA != 0);
        assert!(caps & CAP_COMISION != 0);
    }

    assert!(caps & CAP_UNITARIA != 0);

    let cantidad = next_decimal!();
    let stock = next_decimal!();
    let _ = validar_linea(caps, cantidad, stock);

    let caduce = next_u64!() as i64;
    let ahora = next_u64!() as i64;
    let _ = validar_vigencia(caduce, ahora);

    let dir = std::env::temp_dir().join(format!("fuzz_{}", next_u64!()));
    let _ = std::fs::remove_dir_all(&dir);

    if let Ok(db) = Database::abrir(dir.to_str().unwrap()) {
        let config = ConfigNegocio {
            nombre: Nombre::new("FuzzTest").unwrap_or_else(|_| Nombre::empty()),
            rubros,
            pin_dueno_sha256: "".into(),
        };
        let _ = db.guardar_config(&config);

        let n_productos = (next_u8!() % 10) + 1;
        let mut skus_set: HashSet<String> = HashSet::new();

        for _ in 0..n_productos {
            let sku_str = next_string!(20);
            if skus_set.insert(sku_str.clone()) {
                if let Ok(sku) = Sku::new(&sku_str) {
                    let producto = Producto {
                        sku,
                        nombre: Nombre::new(&next_string!(50)).unwrap_or_else(|_| Nombre::empty()),
                        precio_usd: next_decimal!().abs().max(dec!(0.01)),
                        impuesto_pct: dec!(16),
                        stock: next_decimal!().abs().max(dec!(0)),
                        capacidades: caps,
                    };
                    let _ = db.guardar_producto(&producto);
                }
            }
        }

        if let Ok(catalogo) = db.cargar_catalogo() {
            let n_movs = (next_u8!() % 20) + 1;
            for _ in 0..n_movs {
                if let Some(sku) = skus_set.iter().next().cloned() {
                    let mov = MovimientoStock {
                        id: next_string!(32),
                        sku: sku.clone(),
                        delta: next_decimal!(),
                        motivo: match next_u8!() % 4 {
                            0 => MotivoMovimiento::Venta,
                            1 => MotivoMovimiento::Compra,
                            2 => MotivoMovimiento::Merma,
                            _ => MotivoMovimiento::Ajuste,
                        },
                        venta_id: if next_bool!() { Some(next_string!(32)) } else { None },
                        fecha_unix: next_u64!() as i64,
                        firma_sha256: String::new(),
                    };
                    let _ = db.aplicar_movimiento(mov, |_, _| {});
                }
            }

            if caps & CAP_PERECEDERO != 0 {
                for _ in 0..(next_u8!() % 5) {
                    if let Some(sku) = skus_set.iter().next().cloned() {
                        let lote = Lote {
                            id: next_string!(32),
                            sku: sku.clone(),
                            horneado_unix: next_u64!() as i64,
                            cantidad_inicial: next_decimal!().abs().max(dec!(1)),
                            disponible: next_decimal!().abs().max(dec!(1)),
                            caduce_unix: (next_u64!() % 86400000) as i64 + 86400,
                        };
                        let _ = db.guardar_lote(&lote);
                    }
                }

                if let Ok(mut libro) = db.cargar_lotes() {
                    for _ in 0..5 {
                        if let Some(sku) = skus_set.iter().next().cloned() {
                            let _ = libro.descontar_fefo(&sku, next_decimal!().abs(), next_u64!() as i64);
                        }
                    }
                }
            }
        }

        for _ in 0..(next_u8!() % 5) {
            let mut lineas = LineasVenta::nuevas();
            let tasa = next_decimal!().abs().max(dec!(1));

            for _ in 0..(next_u8!() % 4 + 1) {
                if let Some(sku_str) = skus_set.iter().next().cloned() {
                    if let Ok(sku) = Sku::new(&sku_str) {
                        let nombre = Nombre::new(&next_string!(50)).unwrap_or_else(|_| Nombre::empty());
                        lineas.agregar(
                            sku,
                            nombre,
                            next_decimal!().abs().max(dec!(1)),
                            dec!(1),
                            tasa,
                            "unidad".to_string(),
                        );
                    }
                }
            }

            let venta = Venta {
                id: next_string!(32),
                etiqueta: if next_bool!() { next_string!(30) } else { String::new() },
                es_cuenta_abierta: next_bool!(),
                estado: if next_bool!() { EstadoVenta::Abierta } else { EstadoVenta::Cerrada },
                lineas,
                tasa_del_dia: tasa,
                total_usd: dec!(0),
                total_bs: dec!(0),
                monto_recibido_bs: dec!(0),
                vuelto_bs: dec!(0),
                pagos: Vec::new(),
                estado_vuelto: None,
                metodo_vuelto: None,
                monto_vuelto_usd: None,
                tasa_vuelto: None,
                fecha_apertura_unix: next_u64!() as i64,
                fecha_cierre_unix: if next_bool!() { next_u64!() as i64 } else { 0 },
                firma_sha256: String::new(),
            };
            let _ = db.guardar_venta(venta);
        }

        let backup_path = dir.join("backup.test");
        let _ = db.exportar_backup(backup_path.to_str().unwrap());
        let _ = db.importar_backup(backup_path.to_str().unwrap(), "");
        let _ = db.auto_backup(dir.to_str().unwrap(), 3);
        let _ = db.listar_backups(dir.to_str().unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    if caps & CAP_VARIANTES != 0 {
        for _ in 0..3 {
            let _ = Variante {
                sku: next_string!(32),
                nombre: next_string!(64),
                precio_usd: next_decimal!().abs().max(dec!(0.01)),
                stock: next_decimal!().abs().max(dec!(0)),
                atributos: [AtributoVariante::default(); 8],
                num_atributos: next_u8!() % 8,
            };
        }
    }

    if caps & CAP_GARANTIA != 0 {
        for _ in 0..3 {
            let _ = Garantia {
                sku: next_string!(32),
                serie: [0u8; 64],
                serie_len: next_u8!() % 64,
                fecha_venta_unix: next_u64!() as i64,
                duracion_dias: next_u16!() % 3650,
                estado: match next_u8!() % 4 {
                    0 => EstadoGarantia::Vigente,
                    1 => EstadoGarantia::Vencida,
                    2 => EstadoGarantia::Reclamada,
                    _ => EstadoGarantia::Anulada,
                },
            };
        }
    }

    if caps & CAP_COMISION != 0 {
        for _ in 0..3 {
            let comision = ComisionVendedor {
                vendedor_id: [0u8; 32],
                vendedor_len: next_u8!() % 32,
                porcentaje: next_decimal!().abs().max(dec!(0)).min(dec!(100)),
                monto_acumulado: next_decimal!().abs(),
            };
            let _ = calcular_comision(&comision, next_decimal!().abs().max(dec!(1)));
        }
    }

    let _ = validar_serie(&next_string!(100));
    let _ = validar_atributo(&next_string!(50), &next_string!(100));
    let _ = validar_vendedor(&next_string!(50));
});
