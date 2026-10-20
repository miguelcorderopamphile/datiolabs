use datiolabs_core::models::{Catalogo, LineasVenta, Nombre, Presentacion, Producto, Sku};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::Deserialize;

// Representa exactamente la estructura de transporte HTTP/IPC de Tauri (lib.rs)
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ItemVentaDtoTest {
    sku: String,
    cantidad: String,
    #[serde(default, alias = "modo_venta")]
    modo_venta: Option<String>,
}

#[test]
fn test_deserializacion_transporte_modo_venta_dual() {
    // 1. Caso snake_case: {"sku":"HAR-01","cantidad":"2","modo_venta":"Fardo 20"}
    let json_snake = r#"{"sku":"HAR-01","cantidad":"2","modo_venta":"Fardo 20"}"#;
    let item_snake: ItemVentaDtoTest =
        serde_json::from_str(json_snake).expect("Deserialización snake_case debe ser exitosa");
    assert_eq!(item_snake.modo_venta, Some("Fardo 20".to_string()));

    // 2. Caso camelCase: {"sku":"HAR-01","cantidad":"2","modoVenta":"Fardo 20"}
    let json_camel = r#"{"sku":"HAR-01","cantidad":"2","modoVenta":"Fardo 20"}"#;
    let item_camel: ItemVentaDtoTest =
        serde_json::from_str(json_camel).expect("Deserialización camelCase debe ser exitosa");
    assert_eq!(item_camel.modo_venta, Some("Fardo 20".to_string()));

    // 3. Caso sin modo (default unidad)
    let json_default = r#"{"sku":"HAR-01","cantidad":"1"}"#;
    let item_default: ItemVentaDtoTest =
        serde_json::from_str(json_default).expect("Deserialización default debe ser exitosa");
    assert_eq!(item_default.modo_venta, None);

    // 4. Verificación de regla Serde: campos camelCase con alias snake_case fallan si se envían ambos en el mismo JSON
    let json_ambos =
        r#"{"sku":"HAR-01","cantidad":"2","modo_venta":"Fardo 20","modoVenta":"Fardo 20"}"#;
    let res_ambos: Result<ItemVentaDtoTest, _> = serde_json::from_str(json_ambos);
    assert!(
        res_ambos.is_err(),
        "Serde debe rechazar JSON con ambos alias simultáneos por duplicate field"
    );
}

#[test]
fn test_descuento_exacto_por_presentacion_en_catalogo() {
    let mut cat = Catalogo::nuevo();
    let sku = Sku::new("CERV-01").unwrap();
    let mut prod = Producto::nuevo_simple(
        sku,
        Nombre::new("Cerveza Polar").unwrap(),
        dec!(1.00),
        dec!(16.00),
        dec!(120.00), // 120 unidades en stock
        datiolabs_core::capacidades::CAP_UNITARIA,
    );

    prod.presentaciones.push(Presentacion {
        nombre: "Six Pack".to_string(),
        precio_usd: dec!(5.50),
        unidades: 6,
    });
    prod.presentaciones.push(Presentacion {
        nombre: "Caja 24".to_string(),
        precio_usd: dec!(20.00),
        unidades: 24,
    });

    cat.insertar(prod).unwrap();
    let idx = cat.indice_de("CERV-01").unwrap();

    // Venta de 2 cajas (2 * 24 = 48 unidades a descontar)
    let (_, unidades_caja) = cat
        .presentacion_por_nombre(idx, "Caja 24")
        .expect("Presentacion Caja 24 encontrada");
    assert_eq!(unidades_caja, 24);
    let delta_cajas = Decimal::from(2 * unidades_caja);
    cat.aplicar_delta_stock(idx, -delta_cajas);
    assert_eq!(cat.stock(idx), dec!(72)); // 120 - 48 = 72

    // Venta de 3 Six Packs (3 * 6 = 18 unidades a descontar)
    let (_, unidades_six) = cat
        .presentacion_por_nombre(idx, "Six Pack")
        .expect("Presentacion Six Pack encontrada");
    assert_eq!(unidades_six, 6);
    let delta_six = Decimal::from(3 * unidades_six);
    cat.aplicar_delta_stock(idx, -delta_six);
    assert_eq!(cat.stock(idx), dec!(54)); // 72 - 18 = 54

    // Venta de 4 unidades sueltas
    cat.aplicar_delta_stock(idx, -dec!(4));
    assert_eq!(cat.stock(idx), dec!(50)); // 54 - 4 = 50
}

#[test]
fn test_lineas_venta_registro_y_preservacion_modo() {
    let mut lineas = LineasVenta::nuevas();
    let sku = Sku::new("REF-01").unwrap();
    let nombre = Nombre::new("Refresco 2L").unwrap();

    // Agregar venta de Six Pack
    lineas.agregar(
        sku,
        nombre,
        dec!(2),     // 2 paquetes
        dec!(5.00),  // $5.00 cada Six Pack
        dec!(80.00), // Tasa 80 Bs/$
        "Six Pack".to_string(),
    );

    assert_eq!(lineas.total_usd(), dec!(10.00));
    assert_eq!(lineas.total_bs(), dec!(800.00));
    assert_eq!(
        lineas.modos_venta.first().cloned(),
        Some("Six Pack".to_string())
    );
}

#[test]
fn test_ticket_mixto_multi_presentacion_mismo_sku() {
    let mut cat = Catalogo::nuevo();
    let sku = Sku::new("CERV-POLAR").unwrap();
    let mut prod = Producto::nuevo_simple(
        sku,
        Nombre::new("Cerveza Polar").unwrap(),
        dec!(1.00),
        dec!(16.00),
        dec!(100.00), // Stock inicial: 100 unidades
        datiolabs_core::capacidades::CAP_UNITARIA,
    );

    prod.presentaciones.push(Presentacion {
        nombre: "Six Pack".to_string(),
        precio_usd: dec!(5.50),
        unidades: 6,
    });
    prod.presentaciones.push(Presentacion {
        nombre: "Caja 24".to_string(),
        precio_usd: dec!(20.00),
        unidades: 24,
    });

    cat.insertar(prod).unwrap();
    let idx = cat.indice_de("CERV-POLAR").unwrap();

    let mut lineas = LineasVenta::nuevas();
    let tasa = dec!(80.00);

    // 1. Agregar 2 unidades sueltas
    lineas.agregar(
        sku,
        cat.nombre_obj(idx),
        dec!(2),
        dec!(1.00),
        tasa,
        "unidad".to_string(),
    );

    // 2. Agregar 1 Six Pack (6 unidades)
    lineas.agregar(
        sku,
        cat.nombre_obj(idx),
        dec!(1),
        dec!(5.50),
        tasa,
        "Six Pack".to_string(),
    );

    // 3. Agregar 2 Cajas 24 (2 * 24 = 48 unidades)
    lineas.agregar(
        sku,
        cat.nombre_obj(idx),
        dec!(2),
        dec!(20.00),
        tasa,
        "Caja 24".to_string(),
    );

    // Verificación de líneas SoA independientes (no colapsadas)
    assert_eq!(lineas.skus.len(), 3);
    assert_eq!(lineas.cantidades.len(), 3);
    assert_eq!(lineas.modos_venta.len(), 3);
    assert_eq!(lineas.modos_venta[0], "unidad");
    assert_eq!(lineas.modos_venta[1], "Six Pack");
    assert_eq!(lineas.modos_venta[2], "Caja 24");

    // Totales monetarios:
    // (2 * 1.00) + (1 * 5.50) + (2 * 20.00) = 2.00 + 5.50 + 40.00 = 47.50 USD
    assert_eq!(lineas.total_usd(), dec!(47.50));
    assert_eq!(lineas.total_bs(), dec!(3800.00)); // 47.50 * 80

    // Cálculo y aplicación de descuento de stock total
    // 2 unidades + (1 * 6) + (2 * 24) = 2 + 6 + 48 = 56 unidades físicas
    let mut total_unidades_descontadas = Decimal::ZERO;
    for i in 0..lineas.skus.len() {
        let modo = &lineas.modos_venta[i];
        let cant = lineas.cantidades[i];
        let factor = match modo.as_str() {
            "Six Pack" => 6,
            "Caja 24" => 24,
            _ => 1,
        };
        total_unidades_descontadas += cant * Decimal::from(factor);
    }
    assert_eq!(total_unidades_descontadas, dec!(56));

    cat.aplicar_delta_stock(idx, -total_unidades_descontadas);
    assert_eq!(cat.stock(idx), dec!(44)); // 100 - 56 = 44 unidades restantes

    // Simulación de reversión/eliminación de la línea de Caja 24 (índice 2)
    let modo_removido = lineas.modos_venta.remove(2);
    let cant_removida = lineas.cantidades.remove(2);
    lineas.skus.remove(2);
    lineas.nombres.remove(2);
    lineas.precios_usd.remove(2);
    lineas.tasas_bloqueadas.remove(2);

    let factor_reintegro = match modo_removido.as_str() {
        "Caja 24" => 24,
        "Six Pack" => 6,
        _ => 1,
    };
    let delta_reintegro = cant_removida * Decimal::from(factor_reintegro);
    assert_eq!(delta_reintegro, dec!(48)); // 2 cajas * 24 = 48

    cat.aplicar_delta_stock(idx, delta_reintegro);
    assert_eq!(cat.stock(idx), dec!(92)); // 44 + 48 = 92 unidades exactas
}

#[test]
fn test_desglose_y_auditoria_detallada_presentaciones_en_lineas() {
    let mut lineas = LineasVenta::nuevas();
    let sku = Sku::new("CERV-01").unwrap();
    let nombre = Nombre::new("Cerveza Polar").unwrap();
    let tasa = dec!(80.00);

    // Venta de 2 Six Packs (factor 6) = 12 unidades
    lineas.agregar(
        sku,
        nombre,
        dec!(2),
        dec!(5.50),
        tasa,
        "Six Pack".to_string(),
    );
    // Venta de 3 Cajas 24 (factor 24) = 72 unidades
    lineas.agregar(
        sku,
        nombre,
        dec!(3),
        dec!(20.00),
        tasa,
        "Caja 24".to_string(),
    );
    // Venta de 5 Unidades sueltas (factor 1) = 5 unidades
    lineas.agregar(sku, nombre, dec!(5), dec!(1.00), tasa, "unidad".to_string());

    assert_eq!(lineas.skus.len(), 3);
    assert_eq!(lineas.modos_venta[0], "Six Pack");
    assert_eq!(lineas.cantidades[0], dec!(2));
    assert_eq!(lineas.modos_venta[1], "Caja 24");
    assert_eq!(lineas.cantidades[1], dec!(3));
    assert_eq!(lineas.modos_venta[2], "unidad");
    assert_eq!(lineas.cantidades[2], dec!(5));

    // Validar cálculo exacto de unidades físicas contenidas por cada línea
    let desglose_unidades: Vec<(String, Decimal, u32, Decimal)> = (0..lineas.skus.len())
        .map(|i| {
            let modo = &lineas.modos_venta[i];
            let cant = lineas.cantidades[i];
            let factor = match modo.as_str() {
                "Six Pack" => 6,
                "Caja 24" => 24,
                _ => 1,
            };
            (modo.clone(), cant, factor, cant * Decimal::from(factor))
        })
        .collect();

    assert_eq!(
        desglose_unidades[0],
        ("Six Pack".to_string(), dec!(2), 6, dec!(12))
    );
    assert_eq!(
        desglose_unidades[1],
        ("Caja 24".to_string(), dec!(3), 24, dec!(72))
    );
    assert_eq!(
        desglose_unidades[2],
        ("unidad".to_string(), dec!(5), 1, dec!(5))
    );

    let total_unidades_fisicas: Decimal = desglose_unidades.iter().map(|(_, _, _, tot)| tot).sum();
    assert_eq!(total_unidades_fisicas, dec!(89)); // 12 + 72 + 5 = 89 unidades físicas vendidas
}

#[test]
fn test_liquidacion_cuenta_abierta_y_cuadratura_acumuladores_jornada() {
    use datiolabs_core::modulos::licoreria;

    let mut lineas = LineasVenta::nuevas();
    let tasa_consumo = dec!(807.39);
    let sku = Sku::new("CERV-POL").unwrap();
    let nom = Nombre::new("Cerveza Polar").unwrap();

    // 1. Agregar consumos sucesivos a la cuenta
    lineas.agregar(
        sku,
        nom,
        dec!(4),
        dec!(1.50),
        tasa_consumo,
        "unidad".to_string(),
    ); // $6.00
    lineas.agregar(
        sku,
        nom,
        dec!(2),
        dec!(8.00),
        tasa_consumo,
        "Six Pack".to_string(),
    ); // $16.00

    // Total de consumos: $22.00
    let liquidacion = licoreria::liquidar_cierre(&lineas);
    assert_eq!(liquidacion.total_usd, dec!(22.00));
    assert_eq!(
        liquidacion.total_bs,
        (dec!(22.00) * tasa_consumo).round_dp(2)
    );

    // 2. Simulación de abonos parciales previos: $5.00
    let abonos_previos = dec!(5.00);
    let saldo_pendiente_usd = if abonos_previos > liquidacion.total_usd {
        Decimal::ZERO
    } else {
        liquidacion.total_usd - abonos_previos
    };
    assert_eq!(saldo_pendiente_usd, dec!(17.00));

    // 3. Verificación de actualización en jornada activa
    let mut jornada_total_usd = dec!(100.00);
    let mut jornada_total_bs = (dec!(100.00) * tasa_consumo).round_dp(2);
    let mut jornada_tickets = 5u32;

    // Al cerrar la cuenta, la jornada debe acumular exactamente el neto liquidado
    jornada_total_usd += saldo_pendiente_usd;
    jornada_total_bs += (saldo_pendiente_usd * tasa_consumo).round_dp(2);
    jornada_tickets += 1;

    assert_eq!(jornada_total_usd, dec!(117.00));
    assert_eq!(jornada_tickets, 6);
    assert_eq!(jornada_total_bs, (dec!(117.00) * tasa_consumo).round_dp(2));
}

#[test]
fn test_deteccion_transacciones_fuera_de_jornada_auditoria() {
    // Simula el filtro de auditoria de jornadas:
    let inicio_jornada_unix: i64 = 1726646400; // 08:00:00
    let fin_jornada_unix: i64 = 1726675200; // 16:00:00

    let ticket_en_turno_unix: i64 = 1726650000;
    let ticket_fuera_turno_unix: i64 = 1726690000;

    let esta_en_jornada = |ts: i64| ts >= inicio_jornada_unix && ts <= fin_jornada_unix;

    assert!(esta_en_jornada(ticket_en_turno_unix));
    assert!(!esta_en_jornada(ticket_fuera_turno_unix));
}

#[test]
fn test_agregacion_rigurosa_tickets_por_operadores_individuales_y_relevo() {
    use std::collections::HashMap;

    struct TicketSimulado {
        operador: String,
        total_usd: Decimal,
    }

    let tickets = vec![
        TicketSimulado {
            operador: "Elismar, Luisin".to_string(),
            total_usd: dec!(20.00),
        },
        TicketSimulado {
            operador: "Elismar, Luisin".to_string(),
            total_usd: dec!(30.00),
        },
        TicketSimulado {
            operador: "Luisin".to_string(),
            total_usd: dec!(15.00),
        },
        TicketSimulado {
            operador: "Elismar".to_string(),
            total_usd: dec!(25.00),
        },
    ];

    let mut agrupado: HashMap<String, (u32, Decimal)> = HashMap::new();
    for t in &tickets {
        let entry = agrupado
            .entry(t.operador.clone())
            .or_insert((0, Decimal::ZERO));
        entry.0 += 1;
        entry.1 += t.total_usd;
    }

    assert_eq!(agrupado.len(), 3);
    assert_eq!(agrupado.get("Elismar, Luisin"), Some(&(2, dec!(50.00))));
    assert_eq!(agrupado.get("Luisin"), Some(&(1, dec!(15.00))));
    assert_eq!(agrupado.get("Elismar"), Some(&(1, dec!(25.00))));

    let total_tickets: u32 = agrupado.values().map(|(count, _)| count).sum();
    let total_usd: Decimal = agrupado.values().map(|(_, usd)| usd).sum();

    assert_eq!(total_tickets, 4);
    assert_eq!(total_usd, dec!(90.00));
}
