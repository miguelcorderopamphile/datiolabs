use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

mod serde_vec_str {
    use rust_decimal::Decimal;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::str::FromStr;

    pub fn serialize<S: Serializer>(valores: &[Decimal], s: S) -> Result<S::Ok, S::Error> {
        valores
            .iter()
            .map(Decimal::to_string)
            .collect::<Vec<String>>()
            .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Decimal>, D::Error> {
        let crudo = Vec::<String>::deserialize(d)?;
        crudo
            .iter()
            .map(|t| Decimal::from_str(t).map_err(serde::de::Error::custom))
            .collect()
    }
}

mod serde_opt_decimal_str {
    use rust_decimal::Decimal;
    use serde::{Deserialize, Deserializer, Serializer};
    use std::str::FromStr;

    pub fn serialize<S: Serializer>(val: &Option<Decimal>, s: S) -> Result<S::Ok, S::Error> {
        match val {
            Some(d) => s.serialize_some(&d.to_string()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Decimal>, D::Error> {
        let opt = Option::<String>::deserialize(d)?;
        match opt {
            Some(s) => Decimal::from_str(&s)
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

pub const MAX_SKU_LEN: usize = 32;
pub const MAX_NOMBRE_LEN: usize = 128;
pub const MAX_FUENTE_LEN: usize = 32;
pub const MAX_FIRMA_LEN: usize = 64;
pub const MAX_ETIQUETA_LEN: usize = 64;
pub const MAX_ID_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sku([u8; MAX_SKU_LEN], u8);

impl Sku {
    pub fn new(s: &str) -> Result<Self, &'static str> {
        if s.is_empty() || s.len() > MAX_SKU_LEN {
            return Err("SKU invalido: longitud 1-32");
        }
        let mut buf = [0u8; MAX_SKU_LEN];
        buf[..s.len()].copy_from_slice(s.as_bytes());
        Ok(Self(buf, s.len() as u8))
    }
    pub fn empty() -> Self {
        Self([0u8; MAX_SKU_LEN], 0)
    }
    pub fn from_slice(bytes: &[u8]) -> Self {
        let len = bytes.len().min(MAX_SKU_LEN);
        let mut buf = [0u8; MAX_SKU_LEN];
        buf[..len].copy_from_slice(&bytes[..len]);
        Self(buf, len as u8)
    }
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0[..self.1 as usize]).unwrap_or("")
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0[..self.1 as usize]
    }
}

impl Serialize for Sku {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.as_str().serialize(s)
    }
}

impl<'de> Deserialize<'de> for Sku {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Sku::new(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nombre([u8; MAX_NOMBRE_LEN], u8);

impl Nombre {
    pub fn new(s: &str) -> Result<Self, &'static str> {
        if s.is_empty() || s.len() > MAX_NOMBRE_LEN {
            return Err("Nombre invalido: longitud 1-128");
        }
        let mut buf = [0u8; MAX_NOMBRE_LEN];
        buf[..s.len()].copy_from_slice(s.as_bytes());
        Ok(Self(buf, s.len() as u8))
    }
    pub fn empty() -> Self {
        Self([0u8; MAX_NOMBRE_LEN], 0)
    }
    pub fn from_slice(bytes: &[u8]) -> Self {
        let len = bytes.len().min(MAX_NOMBRE_LEN);
        let mut buf = [0u8; MAX_NOMBRE_LEN];
        buf[..len].copy_from_slice(&bytes[..len]);
        Self(buf, len as u8)
    }
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0[..self.1 as usize]).unwrap_or("")
    }
}

impl Serialize for Nombre {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.as_str().serialize(s)
    }
}

impl<'de> Deserialize<'de> for Nombre {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Nombre::new(&s).map_err(serde::de::Error::custom)
    }
}

/// The fundamental building block of DatioLabs.
/// Immutable BCV exchange rate event. Every persisted rate is cryptographically
/// signed and anchored as the single pricing authority (USD-anchored retail).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventoTasaBcv {
    #[serde(with = "rust_decimal::serde::str")]
    pub valor_bs_por_usd: Decimal,
    pub fecha_unix: i64,
    pub fuente: [u8; MAX_FUENTE_LEN],
    pub fuente_len: u8,
    pub firma_sha256: String,
}

/// Presentacion de venta adicional de un producto (empaque, six-pack, caja, servicio combinado, etc.)
/// unidades: cuantas unidades base del stock descuenta esta presentacion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Presentacion {
    pub nombre: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub precio_usd: Decimal,
    pub unidades: u32,
}

/// Autoritative price anchor is USD only. Bolivares are always derived.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Producto {
    pub sku: Sku,
    pub nombre: Nombre,
    #[serde(with = "rust_decimal::serde::str")]
    pub precio_usd: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub impuesto_pct: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub stock: Decimal,
    pub capacidades: u16,
    #[serde(default)]
    pub categoria_id: Option<String>,
    #[serde(default, with = "serde_opt_decimal_str")]
    pub precio_bruto_usd: Option<Decimal>,
    #[serde(default, with = "serde_opt_decimal_str")]
    pub margen_pct: Option<Decimal>,
    #[serde(default)]
    pub sin_stock: bool,
    #[serde(default)]
    pub unidad: Option<String>,
    #[serde(default)]
    pub es_caja: bool,
    #[serde(default)]
    pub unidades_por_caja: Option<u32>,
    #[serde(default, with = "serde_opt_decimal_str")]
    pub precio_paquete_usd: Option<Decimal>,
    #[serde(default)]
    pub nombre_paquete: Option<String>,
    #[serde(default)]
    pub presentaciones: Vec<Presentacion>,
}

impl Producto {
    pub fn nuevo_simple(
        sku: Sku,
        nombre: Nombre,
        precio_usd: Decimal,
        impuesto_pct: Decimal,
        stock: Decimal,
        capacidades: u16,
    ) -> Self {
        Self {
            sku,
            nombre,
            precio_usd,
            impuesto_pct,
            stock,
            capacidades,
            categoria_id: None,
            precio_bruto_usd: None,
            margen_pct: None,
            sin_stock: false,
            unidad: None,
            es_caja: false,
            unidades_por_caja: None,
            precio_paquete_usd: None,
            nombre_paquete: None,
            presentaciones: Vec::new(),
        }
    }
}

/// Struct-of-Arrays catalog: column-contiguous layout saturating cache lines
/// during grid scans (price refresh, stock sweeps). No per-row indirection.
#[derive(Debug, Clone, Default)]
pub struct Catalogo {
    skus: Vec<Sku>,
    nombres: Vec<Nombre>,
    precios_usd: Vec<Decimal>,
    impuestos_pct: Vec<Decimal>,
    stocks: Vec<Decimal>,
    capacidades: Vec<u16>,
    categorias_id: Vec<Option<String>>,
    precios_bruto_usd: Vec<Option<Decimal>>,
    margenes_pct: Vec<Option<Decimal>>,
    sin_stocks: Vec<bool>,
    unidades: Vec<Option<String>>,
    es_cajas: Vec<bool>,
    unidades_por_cajas: Vec<Option<u32>>,
    precios_paquete_usd: Vec<Option<Decimal>>,
    nombres_paquete: Vec<Option<String>>,
    presentaciones: Vec<Vec<Presentacion>>,
}

impl Catalogo {
    pub fn nuevo() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.skus.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skus.is_empty()
    }

    pub fn insertar(&mut self, p: Producto) -> Result<usize, crate::capacidades::ErrorNegocio> {
        if self.indice_de(p.sku.as_str()).is_some() {
            return Err(crate::capacidades::ErrorNegocio::SkuDuplicado(
                p.sku.as_str().to_string(),
            ));
        }
        let idx = self.skus.len();
        self.skus.push(p.sku);
        self.nombres.push(p.nombre);
        self.precios_usd.push(p.precio_usd);
        self.impuestos_pct.push(p.impuesto_pct);
        self.stocks.push(p.stock);
        self.capacidades.push(p.capacidades);
        self.categorias_id.push(p.categoria_id);
        self.precios_bruto_usd.push(p.precio_bruto_usd);
        self.margenes_pct.push(p.margen_pct);
        self.sin_stocks.push(p.sin_stock);
        self.unidades.push(p.unidad);
        self.es_cajas.push(p.es_caja);
        self.unidades_por_cajas.push(p.unidades_por_caja);
        self.precios_paquete_usd.push(p.precio_paquete_usd);
        self.nombres_paquete.push(p.nombre_paquete);
        self.presentaciones.push(p.presentaciones);
        Ok(idx)
    }

    pub fn indice_de(&self, sku: &str) -> Option<usize> {
        self.skus.iter().position(|s| s.as_str() == sku)
    }

    pub fn sku(&self, idx: usize) -> &str {
        self.skus[idx].as_str()
    }

    pub fn sku_obj(&self, idx: usize) -> Sku {
        self.skus[idx]
    }

    pub fn nombre(&self, idx: usize) -> &str {
        self.nombres[idx].as_str()
    }

    pub fn nombre_obj(&self, idx: usize) -> Nombre {
        self.nombres[idx]
    }

    pub fn precio_usd(&self, idx: usize) -> Decimal {
        self.precios_usd[idx]
    }

    pub fn impuesto_pct(&self, idx: usize) -> Decimal {
        self.impuestos_pct[idx]
    }

    pub fn stock(&self, idx: usize) -> Decimal {
        self.stocks[idx]
    }

    pub fn capacidades(&self, idx: usize) -> u16 {
        self.capacidades[idx]
    }

    pub fn categoria_id(&self, idx: usize) -> Option<String> {
        self.categorias_id[idx].clone()
    }

    pub fn precio_bruto_usd(&self, idx: usize) -> Option<Decimal> {
        self.precios_bruto_usd[idx]
    }

    pub fn margen_pct(&self, idx: usize) -> Option<Decimal> {
        self.margenes_pct[idx]
    }

    pub fn sin_stock(&self, idx: usize) -> bool {
        self.sin_stocks[idx]
    }

    pub fn unidad(&self, idx: usize) -> Option<String> {
        self.unidades[idx].clone()
    }

    pub fn es_caja(&self, idx: usize) -> bool {
        self.es_cajas[idx]
    }

    pub fn unidades_por_caja(&self, idx: usize) -> Option<u32> {
        self.unidades_por_cajas[idx]
    }

    pub fn precio_paquete_usd(&self, idx: usize) -> Option<Decimal> {
        self.precios_paquete_usd[idx]
    }

    pub fn nombre_paquete(&self, idx: usize) -> Option<String> {
        self.nombres_paquete[idx].clone()
    }

    pub fn presentaciones(&self, idx: usize) -> &[Presentacion] {
        &self.presentaciones[idx]
    }

    /// Busca una presentacion por nombre (case-insensitive) y retorna (precio_usd, unidades)
    pub fn presentacion_por_nombre(&self, idx: usize, nombre: &str) -> Option<(Decimal, u32)> {
        self.presentaciones[idx]
            .iter()
            .find(|p| p.nombre.eq_ignore_ascii_case(nombre))
            .map(|p| (p.precio_usd, p.unidades))
    }

    /// Zero-copy in-place stock mutation over the contiguous column.
    /// Stock is clamped to zero minimum — never goes negative.
    pub fn aplicar_delta_stock(&mut self, idx: usize, delta: Decimal) {
        let nuevo = self.stocks[idx] + delta;
        self.stocks[idx] = if nuevo < Decimal::ZERO {
            Decimal::ZERO
        } else {
            nuevo
        };
    }

    /// Column scan valuation of the whole inventory in USD.
    pub fn valor_inventario_usd(&self) -> Decimal {
        self.precios_usd
            .iter()
            .zip(self.stocks.iter())
            .fold(Decimal::ZERO, |acc, (p, s)| acc + *p * *s)
    }

    pub fn a_producto(&self, idx: usize) -> Producto {
        Producto {
            sku: self.skus[idx],
            nombre: self.nombres[idx],
            precio_usd: self.precios_usd[idx],
            impuesto_pct: self.impuestos_pct[idx],
            stock: self.stocks[idx],
            capacidades: self.capacidades[idx],
            categoria_id: self.categorias_id[idx].clone(),
            precio_bruto_usd: self.precios_bruto_usd[idx],
            margen_pct: self.margenes_pct[idx],
            sin_stock: self.sin_stocks[idx],
            unidad: self.unidades[idx].clone(),
            es_caja: self.es_cajas[idx],
            unidades_por_caja: self.unidades_por_cajas[idx],
            precio_paquete_usd: self.precios_paquete_usd[idx],
            nombre_paquete: self.nombres_paquete[idx].clone(),
            presentaciones: self.presentaciones[idx].clone(),
        }
    }

    pub fn desde_productos(filas: Vec<Producto>) -> Self {
        let mut c = Self::nuevo();
        for p in filas {
            let _ = c.insertar(p);
        }
        c
    }
}

/// Struct-of-Arrays sale lines. Open tabs append columns over time while each
/// line freezes its own BCV rate at consumption time (locked-rate policy).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LineasVenta {
    pub skus: Vec<Sku>,
    pub nombres: Vec<Nombre>,
    #[serde(with = "serde_vec_str")]
    pub cantidades: Vec<Decimal>,
    #[serde(with = "serde_vec_str")]
    pub precios_usd: Vec<Decimal>,
    #[serde(with = "serde_vec_str")]
    pub tasas_bloqueadas: Vec<Decimal>,
    #[serde(default)]
    pub modos_venta: Vec<String>,
}

impl LineasVenta {
    pub fn nuevas() -> Self {
        Self::default()
    }

    pub fn agregar(
        &mut self,
        sku: Sku,
        nombre: Nombre,
        cantidad: Decimal,
        precio_usd: Decimal,
        tasa_bloqueada: Decimal,
        modo_venta: String,
    ) {
        self.skus.push(sku);
        self.nombres.push(nombre);
        self.cantidades.push(cantidad);
        self.precios_usd.push(precio_usd);
        self.tasas_bloqueadas.push(tasa_bloqueada);
        self.modos_venta.push(modo_venta);
    }

    pub fn total_usd(&self) -> Decimal {
        self.cantidades
            .iter()
            .zip(self.precios_usd.iter())
            .fold(Decimal::ZERO, |acc, (q, p)| acc + *q * *p)
    }

    /// Locked-rate settlement: heterogeneous rates coexist across lines.
    pub fn total_bs(&self) -> Decimal {
        self.cantidades
            .iter()
            .zip(self.precios_usd.iter())
            .zip(self.tasas_bloqueadas.iter())
            .fold(Decimal::ZERO, |acc, ((q, p), t)| acc + *q * *p * *t)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EstadoVenta {
    Abierta,
    Cerrada,
}

/// Redondeo voluntario del cliente: excedente que deja "a favor" al cobrar o
/// liquidar. Se registra como ingreso del negocio en un arbol propio para no
/// alterar el esquema de `Venta` ni de `Jornada`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Redondeo {
    pub id: String,
    pub venta_id: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub monto_bs: Decimal,
    #[serde(default, with = "serde_opt_decimal_str")]
    pub monto_usd: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::str")]
    pub tasa: Decimal,
    pub fecha_unix: i64,
    #[serde(default)]
    pub jornada_id: Option<String>,
    #[serde(default)]
    pub operador: Option<String>,
    #[serde(default)]
    pub metodo: Option<String>,
    #[serde(default)]
    pub firma_sha256: String,
}

/// Registro de auditoria de una venta anulada. Se guarda en el arbol
/// `anulaciones` con la venta original completa, para que el dinero no cuente
/// como ganancia pero quede constancia de que se elimino y con que turno.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anulacion {
    pub id: String,
    pub venta: Venta,
    pub fecha_anulacion_unix: i64,
    #[serde(default)]
    pub jornada_id: Option<String>,
    #[serde(default)]
    pub jornada_origen_id: Option<String>,
    #[serde(default)]
    pub operador: Option<String>,
    #[serde(default)]
    pub motivo: Option<String>,
    #[serde(default)]
    pub firma_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PagoVenta {
    pub metodo: String,
    pub moneda: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub monto_usd: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub monto_bs: Decimal,
    #[serde(default, with = "serde_opt_decimal_str")]
    pub tasa_cambio: Option<Decimal>,
    #[serde(default)]
    pub referencia: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Venta {
    pub id: String,
    pub etiqueta: String,
    pub es_cuenta_abierta: bool,
    pub estado: EstadoVenta,
    pub lineas: LineasVenta,
    #[serde(with = "rust_decimal::serde::str")]
    pub tasa_del_dia: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub total_usd: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub total_bs: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub monto_recibido_bs: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub vuelto_bs: Decimal,
    #[serde(default)]
    pub pagos: Vec<PagoVenta>,
    #[serde(default)]
    pub estado_vuelto: Option<String>,
    #[serde(default)]
    pub metodo_vuelto: Option<String>,
    #[serde(default, with = "serde_opt_decimal_str")]
    pub monto_vuelto_usd: Option<Decimal>,
    #[serde(default, with = "serde_opt_decimal_str")]
    pub tasa_vuelto: Option<Decimal>,
    pub fecha_apertura_unix: i64,
    pub fecha_cierre_unix: i64,
    pub firma_sha256: String,
    #[serde(default)]
    pub tipo: String,
    #[serde(default)]
    pub cliente: Option<String>,
    #[serde(default)]
    pub nota: Option<String>,
    #[serde(default, with = "serde_opt_decimal_str")]
    pub abonos_usd: Option<Decimal>,
    #[serde(default, with = "serde_opt_decimal_str")]
    pub abonos_bs: Option<Decimal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MotivoMovimiento {
    Venta,
    Compra,
    Merma,
    Ajuste,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovimientoStock {
    pub id: String,
    pub sku: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub delta: Decimal,
    pub motivo: MotivoMovimiento,
    pub venta_id: Option<String>,
    pub fecha_unix: i64,
    pub firma_sha256: String,
}

/// Business configuration written once by the first-run wizard. Blank start:
/// absence of this record equals factory state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigNegocio {
    pub nombre: Nombre,
    pub rubros: u16,
    pub pin_dueno_sha256: String,
    #[serde(default)]
    pub licencia_clave: String,
    #[serde(default)]
    pub licencia_titular: String,
    #[serde(default)]
    pub licencia_estado: String,
    #[serde(default)]
    pub privacidad_inventario: bool,
}

impl ConfigNegocio {
    pub fn nuevo_simple(nombre: Nombre, rubros: u16, pin_dueno_sha256: String) -> Self {
        Self {
            nombre,
            rubros,
            pin_dueno_sha256,
            licencia_clave: String::new(),
            licencia_titular: String::new(),
            licencia_estado: String::new(),
            privacidad_inventario: false,
        }
    }
}

// ---------------- modelos nuevos para gestion ----------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Categoria {
    pub id: String,
    pub nombre: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TasaImpuesto {
    pub id: String,
    pub nombre: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub porcentaje: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetodoPagoConfig {
    pub nombre: String,
    pub moneda: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Operador {
    pub id: String,
    pub nombre: String,
    pub activo: bool,
    pub creado_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Jornada {
    pub id: String,
    pub estado: String,
    pub inicio_unix: i64,
    pub fin_unix: Option<i64>,
    pub operador_inicial: String,
    pub operador_actual: String,
    pub operadores_activos: Vec<String>,
    pub operadores_relevo: Vec<String>,
    #[serde(with = "rust_decimal::serde::str")]
    pub tasa_inicio: Decimal,
    #[serde(default, with = "serde_opt_decimal_str")]
    pub tasa_fin: Option<Decimal>,
    #[serde(with = "rust_decimal::serde::str")]
    pub ventas_total_usd: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub ventas_total_bs: Decimal,
    pub tickets_emitidos: u32,
    #[serde(with = "rust_decimal::serde::str")]
    pub vuelto_pagado_bs: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub vuelto_retenido_bs: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub deudas_liquidadas_usd: Decimal,
    pub entradas_stock_reg: u32,
    pub mermas_stock_reg: u32,
    pub cambios_precio_reg: u32,
    #[serde(default)]
    pub checksum_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemaforoStock {
    pub rojo_max: u32,
    pub amarillo_max: u32,
}
