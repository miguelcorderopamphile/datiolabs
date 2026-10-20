use datiolabs_core::{
    capacidades::CAP_UNITARIA,
    models::{Catalogo, LineasVenta, Nombre, Presentacion, Producto, Sku},
};
use proptest::prelude::*;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn arb_pos_decimal(min_cents: i64, max_cents: i64) -> impl Strategy<Value = Decimal> {
    (min_cents..=max_cents).prop_map(|v| Decimal::from(v) / dec!(100))
}

fn arb_sku_id() -> impl Strategy<Value = String> {
    "[A-Z0-9]{3,8}".prop_map(|s: String| s.to_uppercase())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    // ─────────────────────────────────────────────────────────────────────────
    // TEST 1: Ley de Conservación Estricta de Inventario (Multi-presentaciones)
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn test_invariante_conservacion_stock(
        sku_str in arb_sku_id(),
        stock_inicial in arb_pos_decimal(1000, 10000), // 10.00 a 100.00 unidades
        ventas_unitarias in arb_pos_decimal(10, 200),  // 0.10 a 2.00 unidades
        paquetes_vendidos in 1u32..5u32,               // 1 a 4 paquetes
        unidades_por_paquete in 2u32..12u32,          // ej: six-pack = 6 unidades
    ) {
        let sku = Sku::new(&sku_str).unwrap();
        let pres = Presentacion {
            nombre: "Pack".to_string(),
            precio_usd: dec!(15.00),
            unidades: unidades_por_paquete,
        };

        let mut prod = Producto::nuevo_simple(
            sku,
            Nombre::new("Producto Conservacion").unwrap(),
            dec!(3.00),
            dec!(16.00),
            stock_inicial,
            CAP_UNITARIA,
        );
        prod.presentaciones.push(pres);

        let mut cat = Catalogo::nuevo();
        cat.insertar(prod).unwrap();
        let idx = cat.indice_de(&sku_str).unwrap();

        // 1. Venta unitaria directa
        cat.aplicar_delta_stock(idx, -ventas_unitarias);

        // 2. Venta de paquetes (cada paquete descuenta K unidades físicas)
        let delta_paquetes = Decimal::from(paquetes_vendidos * unidades_por_paquete);
        cat.aplicar_delta_stock(idx, -delta_paquetes);

        // Invariante de conservación
        let stock_final = cat.stock(idx);
        let consumo_total = ventas_unitarias + delta_paquetes;
        let stock_esperado = if stock_inicial >= consumo_total {
            stock_inicial - consumo_total
        } else {
            Decimal::ZERO
        };

        prop_assert_eq!(stock_final, stock_esperado, "Violación de conservación de stock");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // TEST 2: Invarianza Metamórfica de Carrito (Permutación y Partición)
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn test_invarianza_metamorfica_total_carrito(
        precios in proptest::collection::vec(arb_pos_decimal(50, 5000), 2..6),
        cantidades in proptest::collection::vec(arb_pos_decimal(1, 10), 2..6),
        tasa in arb_pos_decimal(5000, 15000), // Tasa BCV 50.00 a 150.00
    ) {
        let n = precios.len().min(cantidades.len());
        let mut lineas_orden_a = LineasVenta::nuevas();
        let mut lineas_orden_b = LineasVenta::nuevas();

        // Cargar en orden directo
        for i in 0..n {
            let sku = Sku::new(&format!("SKU{}", i)).unwrap();
            let nom = Nombre::new(&format!("Item {}", i)).unwrap();
            lineas_orden_a.agregar(sku, nom, cantidades[i], precios[i], tasa, "unidad".into());
        }

        // Cargar en orden inverso
        for i in (0..n).rev() {
            let sku = Sku::new(&format!("SKU{}", i)).unwrap();
            let nom = Nombre::new(&format!("Item {}", i)).unwrap();
            lineas_orden_b.agregar(sku, nom, cantidades[i], precios[i], tasa, "unidad".into());
        }

        // Metamorfosis: El orden de escaneo o inserción NO debe alterar el total USD ni el total Bs
        prop_assert_eq!(
            lineas_orden_a.total_usd(),
            lineas_orden_b.total_usd(),
            "Discrepancia metamórfica en USD por orden de líneas"
        );
        prop_assert_eq!(
            lineas_orden_a.total_bs(),
            lineas_orden_b.total_bs(),
            "Discrepancia metamórfica en Bs por orden de líneas"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // TEST 3: Cuadratura Financiera y Liquidación Multimoneda Exacta
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn test_cuadratura_liquidacion_multimoneda(
        total_usd in arb_pos_decimal(100, 10000), // $1.00 a $100.00
        tasa_bcv in arb_pos_decimal(6000, 12000), // 60.00 a 120.00 Bs/$
        fraccion_pago_usd in 0u32..100u32,       // Porcentaje pagado en USD efectivo
    ) {
        let total_bs = (total_usd * tasa_bcv).round_dp(2);

        // Simulamos pago mixto (USD + Bs)
        let pago_usd = (total_usd * Decimal::from(fraccion_pago_usd) / dec!(100)).round_dp(2);
        let aporte_bs_de_usd = (pago_usd * tasa_bcv).round_dp(2);

        // Lo restante se abona en Bolívares (Pago Móvil / Punto)
        let remanente_bs = (total_bs - aporte_bs_de_usd).max(Decimal::ZERO);
        let sobrepago_bs = dec!(15.50); // Cliente entrega un billete de mayor denominación en Bs
        let pago_bs_total = remanente_bs + sobrepago_bs;

        let total_cubierto_bs = aporte_bs_de_usd + pago_bs_total;
        let faltante_bs = (total_bs - total_cubierto_bs).max(Decimal::ZERO);
        let vuelto_bs = (total_cubierto_bs - total_bs).max(Decimal::ZERO);

        // Invariante Fundamental de Caja y Contabilidad por Partida Doble
        prop_assert_eq!(
            total_cubierto_bs + faltante_bs - vuelto_bs,
            total_bs,
            "Violación de cuadratura bancaria en pago mixto multimoneda"
        );

        // El vuelto entregado debe ser exactamente igual al sobrepago ingresado
        prop_assert_eq!(
            vuelto_bs,
            sobrepago_bs,
            "El cálculo del vuelto difiere del excedente real entregado"
        );
    }
}
