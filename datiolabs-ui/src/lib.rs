mod error;
mod tasa_bcv;

use datiolabs_core::capacidades::{
    self, ErrorNegocio, capacidades_de_rubros, rubros_activos, validar_linea,
};
use datiolabs_core::db::{Database as Ledger, DbError};
use datiolabs_core::models::{
    Anulacion, Catalogo, Categoria, ConfigNegocio, EstadoVenta, Jornada, LineasVenta,
    MetodoPagoConfig, MotivoMovimiento, MovimientoStock, Nombre, Operador, PagoVenta, Producto,
    Redondeo, SemaforoStock, Sku, TasaImpuesto, Venta,
};
use datiolabs_core::modulos::licoreria;
use datiolabs_core::modulos::panaderia::Lote;
use error::UIError;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::Manager;

use tasa_bcv::{ServicioTasa, TasaInfo, iniciar_refresco};

use uuid::Uuid;

use std::collections::HashMap;

#[derive(Clone)]
struct AppState {
    ledger: Arc<Mutex<Ledger>>,
    servicio_tasa: Arc<ServicioTasa>,
    dedup_ventas: Arc<std::sync::Mutex<HashMap<String, (i64, String)>>>, // key -> (timestamp, venta_id)
    ultimo_latido: Arc<std::sync::atomic::AtomicI64>,
    cache_deudas: Arc<std::sync::Mutex<Option<(u64, Arc<Vec<DeudaIndice>>)>>>,
}

/// Ficha compacta de una deuda para el buscador. No se persiste: es un indice
/// derivado del arbol de ventas que se reconstruye cuando cambia una deuda.
#[derive(Clone)]
struct DeudaIndice {
    id: String,
    nombre: String,
    normalizado: String,
    cliente_normalizado: String,
    abierta: bool,
    apertura_unix: i64,
    cierre_unix: i64,
    total_usd: Decimal,
    abonos_usd: Decimal,
    saldo_usd: Decimal,
}

fn normalizar_busqueda(texto: &str) -> String {
    texto
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            _ => c,
        })
        .collect()
}

fn ahora_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

fn fecha_vet_str(timestamp_unix: i64, format_str: &str) -> String {
    let tz_vet = chrono::FixedOffset::east_opt(-4 * 3600).unwrap();
    chrono::DateTime::from_timestamp(timestamp_unix, 0)
        .map(|dt| dt.with_timezone(&tz_vet).format(format_str).to_string())
        .unwrap_or_default()
}

fn hash_pin(pin: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(pin.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ---------------- Rate Limiting para PIN ----------------

fn con_ledger<T, E>(
    estado: &AppState,
    operacion: impl FnOnce(&Ledger) -> Result<T, E>,
) -> Result<T, UIError>
where
    E: Into<UIError>,
{
    let guardia = estado.ledger.lock().map_err(|_| {
        UIError::new(
            "almacenamiento bloqueado",
            "El mutex de la base de datos esta envenenado",
        )
    })?;
    operacion(&guardia).map_err(Into::into)
}

// ---------------- DTOs (contrato camelCase con el frontend) ----------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigDto {
    nombre: String,
    rubros: u16,
    capacidades: u16,
    tiene_pin: bool,
    licencia_estado: String,
    licencia_titular: String,
    privacidad_inventario: bool,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PresentacionDto {
    nombre: String,
    precio_usd: String,
    unidades: u32,
}

impl From<&datiolabs_core::models::Presentacion> for PresentacionDto {
    fn from(p: &datiolabs_core::models::Presentacion) -> Self {
        Self {
            nombre: p.nombre.clone(),
            precio_usd: p.precio_usd.to_string(),
            unidades: p.unidades,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProductoInput {
    sku: String,
    nombre: String,
    precio_usd: String,
    impuesto_pct: String,
    stock_inicial: String,
    pesable: bool,
    #[serde(default)]
    perecedero: bool,
    #[serde(default)]
    categoria_id: Option<String>,
    #[serde(default)]
    precio_bruto_usd: Option<String>,
    #[serde(default)]
    margen_pct: Option<String>,
    #[serde(default)]
    sin_stock: Option<bool>,
    #[serde(default)]
    unidad: Option<String>,
    #[serde(default)]
    es_caja: Option<bool>,
    #[serde(default)]
    unidades_por_caja: Option<u32>,
    #[serde(default)]
    precio_paquete_usd: Option<String>,
    #[serde(default)]
    nombre_paquete: Option<String>,
    #[serde(default)]
    presentaciones: Option<Vec<PresentacionDto>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProductoDto {
    sku: Sku,
    nombre: Nombre,
    precio_usd: Decimal,
    impuesto_pct: Decimal,
    stock: Decimal,
    capacidades: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    categoria_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    precio_bruto_usd: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    margen_pct: Option<Decimal>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    sin_stock: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    unidad: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    es_caja: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    unidades_por_caja: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    precio_paquete_usd: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nombre_paquete: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    presentaciones: Vec<PresentacionDto>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemVentaDto {
    sku: String,
    cantidad: String,
    #[serde(default, alias = "modo_venta")]
    modo_venta: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LineaTicketDto {
    sku: Sku,
    nombre: Nombre,
    cantidad: Decimal,
    precio_usd: Decimal,
    tasa_bloqueada: Decimal,
    subtotal_usd: Decimal,
    subtotal_bs: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    modo_venta: Option<String>,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PagoTicketDto {
    metodo: String,
    moneda: Option<String>,
    monto_usd: String,
    monto_bs: String,
    tasa_cambio: Option<String>,
    referencia: Option<String>,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ResolucionVueltoDto {
    estado: String,
    metodo: Option<String>,
    monto_bs: Option<String>,
    monto_usd: Option<String>,
    tasa: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TicketDto {
    venta_id: String,
    lineas: Vec<LineaTicketDto>,
    total_usd: Decimal,
    total_bs: Decimal,
    monto_recibido_bs: Decimal,
    vuelto_bs: Decimal,
    tasa_del_dia: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pagos: Option<Vec<PagoTicketDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    estado_vuelto: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metodo_vuelto: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    monto_vuelto_bs: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    monto_vuelto_usd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tasa_vuelto: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fecha_hora: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    canal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    operador: Option<String>,
    // Nombre de la cuenta o deuda (la `etiqueta` original de la Venta).
    // Se conserva aunque `cliente` se sobreescriba con el operador al cerrar.
    #[serde(skip_serializing_if = "Option::is_none")]
    etiqueta: Option<String>,
    fecha_unix: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConsumoCuentaDto {
    id: String,
    sku: String,
    nombre: String,
    cantidad: Decimal,
    precio_usd: Decimal,
    subtotal_usd: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    modo_venta: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CuentaDto {
    venta_id: String,
    etiqueta: String,
    lineas: usize,
    total_parcial_usd: Decimal,
    total_parcial_bs: Decimal,
    #[serde(skip_serializing_if = "Option::is_none")]
    abonos_usd: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    abonos_bs: Option<Decimal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tipo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cliente: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nota: Option<String>,
    consumos: Vec<ConsumoCuentaDto>,
    fecha_apertura_unix: i64,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct CriticoDto {
    sku: Sku,
    nombre: Nombre,
    stock: Decimal,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct TopProductoDto {
    nombre: Nombre,
    cantidad: Decimal,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CategoriaDineroBrutoDto {
    categoria_id: String,
    nombre: String,
    cantidad_productos: usize,
    unidades_stock: Decimal,
    dinero_bruto_usd: Decimal,
    dinero_bruto_bs: Decimal,
    dinero_venta_usd: Decimal,
    dinero_venta_bs: Decimal,
    margen_bruto_proyectado_usd: Decimal,
    margen_bruto_pct: String,
    porcentaje_capital: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PanelDto {
    ventas_24h_usd: Decimal,
    ventas_24h_bs: Decimal,
    tickets_24h: usize,
    total_productos: usize,
    valor_inventario_usd: Decimal,
    costo_total_usd: Decimal,
    ganancia_bruta_usd: Decimal,
    ganancia_neta_usd: Decimal,
    ganancia_neta_sin_imp_usd: Decimal,
    ganancia_neta_bs: Decimal,
    criticos: Vec<CriticoDto>,
    cuentas_abiertas: usize,
    deudas_abiertas: usize,
    dinero_en_la_calle_usd: Decimal,
    dinero_en_la_calle_bs: Decimal,
    top_productos: Vec<TopProductoDto>,
    dinero_por_categoria: Vec<CategoriaDineroBrutoDto>,
}

fn decimal_de(texto: &str) -> Result<Decimal, UIError> {
    Decimal::from_str(texto.trim().replace(',', ".").as_str()).map_err(|_| {
        UIError::new(
            format!("monto invalido: {texto}"),
            "Se esperaba un numero decimal",
        )
    })
}

/// Exige una jornada abierta para toda operacion que afecte caja o arqueo.
fn jornada_requerida(estado: &AppState) -> Result<(), UIError> {
    let abierta = con_ledger(estado, |db| db.jornada_actual())?.is_some();
    if !abierta {
        return Err(UIError::new(
            "sin turno abierto",
            "Abra un turno antes de cobrar, liquidar o registrar movimientos de caja",
        ));
    }
    Ok(())
}

fn config_requerida(estado: &AppState) -> Result<ConfigNegocio, UIError> {
    con_ledger(estado, |db| db.cargar_config())?.ok_or_else(|| {
        UIError::new(
            "negocio sin inicializar",
            "Ejecute el asistente inicial antes de operar",
        )
    })
}

fn catalogo_fresco(db: &Ledger) -> Result<Catalogo, DbError> {
    db.cargar_catalogo()
}

fn tasa_viva(estado: &AppState) -> Result<Decimal, UIError> {
    let info = estado.servicio_tasa.info_actual();
    if info.valor <= Decimal::ZERO {
        return Err(UIError::new(
            "sin tasa BCV vigente",
            "Actualice la tasa manual o revise la conexion antes de cobrar",
        ));
    }
    Ok(info.valor)
}

fn movimiento(
    sku: &str,
    delta: Decimal,
    motivo: MotivoMovimiento,
    venta_id: Option<String>,
) -> MovimientoStock {
    MovimientoStock {
        id: Uuid::new_v4().to_string(),
        sku: sku.to_string(),
        delta,
        motivo,
        venta_id,
        fecha_unix: ahora_unix(),
        firma_sha256: String::new(),
    }
}

fn descontar_con_lotes_interno(
    db: &Ledger,
    catalogo: &Catalogo,
    idx: usize,
    cantidad: Decimal,
    venta_id: &str,
    modo_venta: &str,
) -> Result<(), DbError> {
    // Resolucion dinamica de unidades a descontar segun la presentacion activa
    let mut unidades = cantidad;
    if let Some((_, upc)) = catalogo.presentacion_por_nombre(idx, modo_venta) {
        if upc > 0 {
            unidades = cantidad * Decimal::from(upc);
        }
    } else if modo_venta == "paquete" && catalogo.es_caja(idx) {
        if let Some(upc) = catalogo.unidades_por_caja(idx) {
            if upc > 0 {
                unidades = cantidad * Decimal::from(upc);
            }
        }
    }
    let es_perecedero = catalogo.capacidades(idx) & capacidades::CAP_PERECEDERO != 0;
    if es_perecedero {
        let sku = catalogo.sku(idx).to_string();
        let mut libro = db.cargar_lotes()?;
        let tocados = libro.descontar_fefo(&sku, unidades, ahora_unix())?;
        for (lote_id, _) in tocados {
            if let Some((_, disp)) = libro.par_disponible_por_id(&lote_id) {
                db.actualizar_disponible_lote(&lote_id, disp)?;
            }
        }
    }
    let mov = movimiento(
        catalogo.sku(idx),
        -unidades,
        MotivoMovimiento::Venta,
        Some(venta_id.to_string()),
    );
    db.aplicar_movimiento(mov, |_, _| {})?;
    Ok(())
}

/// Reverso de lotes para productos perecederos: devuelve las unidades al lote
/// vigente mas proximo a vencer. Si no hay lotes, no bloquea la operacion.
fn reintegrar_lotes_si_perecedero(
    db: &Ledger,
    sku: &str,
    capacidades: u16,
    unidades: Decimal,
) -> Result<(), DbError> {
    if capacidades & capacidades::CAP_PERECEDERO == 0 || unidades <= Decimal::ZERO {
        return Ok(());
    }
    let mut libro = db.cargar_lotes()?;
    let tocados = libro.reintegrar_fefo(sku, unidades, ahora_unix());
    for (lote_id, _) in tocados {
        if let Some((_, disp)) = libro.par_disponible_por_id(&lote_id) {
            db.actualizar_disponible_lote(&lote_id, disp)?;
        }
    }
    Ok(())
}

// ---------------- comandos: configuracion ----------------

#[tauri::command]
fn obtener_config(estado: tauri::State<AppState>) -> Result<Option<ConfigDto>, UIError> {
    let cfg = con_ledger(&estado, |db| db.cargar_config())?;
    Ok(cfg.map(|c| ConfigDto {
        capacidades: capacidades_de_rubros(c.rubros),
        tiene_pin: !c.pin_dueno_sha256.is_empty(),
        nombre: c.nombre.as_str().to_string(),
        rubros: c.rubros,
        licencia_estado: c.licencia_estado,
        licencia_titular: c.licencia_titular,
        privacidad_inventario: c.privacidad_inventario,
    }))
}

#[tauri::command]
fn inicializar_negocio(
    estado: tauri::State<AppState>,
    nombre: String,
    rubros: u8,
    pin_dueno: Option<String>,
    licencia_clave: Option<String>,
    licencia_titular: Option<String>,
    privacidad_inventario: Option<bool>,
) -> Result<(), UIError> {
    let rubros_u16 = rubros as u16;
    if !rubros_activos(rubros_u16) {
        return Err(UIError::new(
            "seleccion de rubros invalida",
            "Active al menos un rubro conocido (abasto, panaderia, licoreria)",
        ));
    }
    let nombre_obj = Nombre::new(&nombre).map_err(|e| UIError::new("nombre invalido", e))?;

    let clave =
        datiolabs_core::capacidades::normalizar_licencia(&licencia_clave.unwrap_or_default());
    let titular = licencia_titular.unwrap_or_default();
    let estado_lic = if clave.is_empty() {
        "demo".to_string()
    } else {
        if !datiolabs_core::capacidades::validar_licencia_universal(&clave) {
            return Err(UIError::new(
                "licencia invalida",
                "La clave de licencia no pertenece al conjunto oficial. Verifique la clave entregada con la compra.",
            ));
        }
        "activa".to_string()
    };

    // PIN vacio significa "panel abierto": se guarda como ausencia de PIN.
    if let Some(ref p) = pin_dueno {
        if !p.trim().is_empty() && p.trim().len() < 6 {
            return Err(UIError::new(
                "pin muy corto",
                "El PIN debe tener al menos 6 dígitos",
            ));
        }
    }

    con_ledger(&estado, |db| {
        if db.cargar_config()?.is_some() {
            return Err(DbError::Negocio(ErrorNegocio::YaInicializado));
        }
        db.guardar_config(&ConfigNegocio {
            nombre: nombre_obj,
            rubros: rubros_u16,
            pin_dueno_sha256: pin_dueno
                .filter(|p| !p.trim().is_empty())
                .map(|p| hash_pin(&p))
                .unwrap_or_default(),
            licencia_clave: clave,
            licencia_titular: titular,
            licencia_estado: estado_lic,
            privacidad_inventario: privacidad_inventario.unwrap_or(false),
        })?;
        let defaults = [
            ("PUNTOD.VENTA", "BS"),
            ("BIOPAGO", "BS"),
            ("PAGO MOVIL", "BS"),
            ("TRANSF.BS.", "BS"),
            ("BS.EFEC.", "BS"),
            ("DOL.CASH", "USD"),
            ("ZELLE", "USD"),
            ("BINAN.USDT", "USD"),
        ];
        for (nombre, moneda) in defaults {
            let _ = db.guardar_metodo_pago(&MetodoPagoConfig {
                nombre: nombre.to_string(),
                moneda: moneda.to_string(),
            });
        }
        Ok::<(), DbError>(())
    })?;
    Ok(())
}

#[tauri::command]
fn validar_pin_dueno(estado: tauri::State<AppState>, pin: String) -> Result<bool, UIError> {
    let cfg = config_requerida(&estado)?;
    Ok(!cfg.pin_dueno_sha256.is_empty() && cfg.pin_dueno_sha256 == hash_pin(&pin))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LicenciaDto {
    estado: String,
    tipo: String,
    clave_licencia: String,
    titular: String,
    validez: String,
}

fn validar_clave_licencia(clave: &str, _rubros: u16) -> bool {
    datiolabs_core::capacidades::validar_licencia_universal(clave)
}

#[tauri::command]
fn generar_licencia() -> Result<String, UIError> {
    Ok(datiolabs_core::capacidades::generar_licencia_universal())
}

#[tauri::command]
fn obtener_licencia(estado: tauri::State<AppState>) -> Result<LicenciaDto, UIError> {
    let cfg = config_requerida(&estado)?;
    let tipo = "DatioLabs Enterprise Universal";
    Ok(LicenciaDto {
        estado: cfg.licencia_estado,
        tipo: tipo.to_string(),
        clave_licencia: cfg.licencia_clave,
        titular: cfg.licencia_titular,
        validez: "Perpetua (Sin caducidad / Portabilidad Total)".to_string(),
    })
}

#[tauri::command]
fn validar_licencia(estado: tauri::State<AppState>, clave: String) -> Result<bool, UIError> {
    let cfg = config_requerida(&estado)?;
    Ok(validar_clave_licencia(&clave, cfg.rubros))
}

// ---------------- comandos: productos ----------------

#[tauri::command]
fn crear_producto(estado: tauri::State<AppState>, input: ProductoInput) -> Result<(), UIError> {
    let cfg = config_requerida(&estado)?;
    let permitidas = capacidades_de_rubros(cfg.rubros);
    let mut caps = capacidades::CAP_UNITARIA;
    if input.pesable {
        caps |= capacidades::CAP_PESABLE;
        if permitidas & capacidades::CAP_PESABLE == 0 {
            return Err(UIError::from(ErrorNegocio::CapacidadInactiva(caps)));
        }
    }
    if input.perecedero {
        caps |= capacidades::CAP_PERECEDERO;
        if permitidas & capacidades::CAP_PERECEDERO == 0 {
            return Err(UIError::from(ErrorNegocio::CapacidadInactiva(caps)));
        }
    }
    let producto = Producto {
        sku: Sku::new(&input.sku.trim().to_uppercase())
            .map_err(|e| UIError::new("SKU invalido", e))?,
        nombre: Nombre::new(input.nombre.trim()).map_err(|e| UIError::new("nombre invalido", e))?,
        precio_usd: decimal_de(&input.precio_usd)?,
        impuesto_pct: decimal_de(&input.impuesto_pct)?,
        stock: decimal_de(&input.stock_inicial)?,
        capacidades: caps,
        categoria_id: input.categoria_id.clone(),
        precio_bruto_usd: input
            .precio_bruto_usd
            .as_deref()
            .and_then(|s| decimal_de(s).ok()),
        margen_pct: input.margen_pct.as_deref().and_then(|s| decimal_de(s).ok()),
        sin_stock: input.sin_stock.unwrap_or(false),
        unidad: input.unidad.clone(),
        es_caja: input.es_caja.unwrap_or(false),
        unidades_por_caja: input.unidades_por_caja,
        precio_paquete_usd: input
            .precio_paquete_usd
            .as_deref()
            .and_then(|s| decimal_de(s).ok()),
        nombre_paquete: input.nombre_paquete.clone(),
        presentaciones: input
            .presentaciones
            .unwrap_or_default()
            .into_iter()
            .filter_map(|p| {
                decimal_de(&p.precio_usd).ok().map(|precio_usd| {
                    datiolabs_core::models::Presentacion {
                        nombre: p.nombre,
                        precio_usd,
                        unidades: p.unidades.max(1),
                    }
                })
            })
            .collect(),
    };
    if producto.sku.as_str().is_empty()
        || producto.nombre.as_str().is_empty()
        || producto.precio_usd <= Decimal::ZERO
        || producto.stock < Decimal::ZERO
    {
        return Err(UIError::new(
            "datos de producto invalidos",
            "SKU y nombre obligatorios; precio mayor a cero; stock no negativo",
        ));
    }
    con_ledger(&estado, |db| db.guardar_producto(&producto))?;
    Ok(())
}

#[tauri::command]
fn listar_productos(estado: tauri::State<AppState>) -> Result<Vec<ProductoDto>, UIError> {
    config_requerida(&estado)?;
    let db = estado
        .ledger
        .lock()
        .map_err(|_| UIError::new("db bloqueada", ""))?;
    let tree = db
        .inner_db()
        .open_tree("productos")
        .map_err(|e| UIError::new("error db", &e.to_string()))?;
    let filas: Vec<ProductoDto> = tree
        .iter()
        .filter_map(|r| r.ok())
        .filter_map(|(_, v)| {
            bincode::deserialize::<Producto>(&v)
                .or_else(|_| serde_json::from_slice::<Producto>(&v))
                .ok()
        })
        .map(|p| ProductoDto {
            sku: p.sku,
            nombre: p.nombre,
            precio_usd: p.precio_usd,
            impuesto_pct: p.impuesto_pct,
            stock: p.stock,
            capacidades: p.capacidades,
            categoria_id: p.categoria_id,
            precio_bruto_usd: p.precio_bruto_usd,
            margen_pct: p.margen_pct,
            sin_stock: p.sin_stock,
            unidad: p.unidad,
            es_caja: p.es_caja,
            unidades_por_caja: p.unidades_por_caja,
            precio_paquete_usd: p.precio_paquete_usd,
            nombre_paquete: p.nombre_paquete,
            presentaciones: p.presentaciones.iter().map(PresentacionDto::from).collect(),
        })
        .collect();
    Ok(filas)
}

#[tauri::command]
fn compra_stock(
    estado: tauri::State<AppState>,
    sku: String,
    cantidad: String,
) -> Result<Decimal, UIError> {
    config_requerida(&estado)?;
    let cant = decimal_de(&cantidad)?;
    if cant <= Decimal::ZERO {
        return Err(UIError::new("cantidad invalida", "Debe ser mayor a cero"));
    }
    let mov = movimiento(
        sku.trim().to_uppercase().as_str(),
        cant,
        MotivoMovimiento::Compra,
        None,
    );
    let nuevo_stock = con_ledger(&estado, |db| -> Result<Decimal, DbError> {
        db.aplicar_movimiento(mov, |_, _| {})?;
        let cat = db.cargar_catalogo()?;
        let idx = cat
            .indice_de(sku.trim().to_uppercase().as_str())
            .ok_or(DbError::Negocio(ErrorNegocio::ProductoInexistente))?;
        Ok(cat.stock(idx))
    })?;
    Ok(nuevo_stock)
}

#[tauri::command]
fn registrar_merma(
    estado: tauri::State<AppState>,
    sku: String,
    cantidad: String,
    lote_id: Option<String>,
) -> Result<Decimal, UIError> {
    config_requerida(&estado)?;
    let cant = decimal_de(&cantidad)?;
    if cant <= Decimal::ZERO {
        return Err(UIError::new("cantidad invalida", "Debe ser mayor a cero"));
    }
    let sku_norm = sku.trim().to_uppercase();

    if let Some(lid) = lote_id {
        let (_, restante) = con_ledger(&estado, |db| -> Result<(String, Decimal), DbError> {
            let mut libro = db.cargar_lotes()?;
            let restante = libro
                .registrar_merma(&lid, cant)
                .map_err(DbError::Negocio)?;
            Ok((lid.clone(), restante))
        })?;
        con_ledger(&estado, |db| db.actualizar_disponible_lote(&lid, restante))?;
    }

    let mov = movimiento(&sku_norm, -cant, MotivoMovimiento::Merma, None);
    let nuevo_stock = con_ledger(&estado, |db| -> Result<Decimal, DbError> {
        db.aplicar_movimiento(mov, |_, _| {})?;
        let cat = db.cargar_catalogo()?;
        let idx = cat
            .indice_de(&sku_norm)
            .ok_or(DbError::Negocio(ErrorNegocio::ProductoInexistente))?;
        Ok(cat.stock(idx))
    })?;
    Ok(nuevo_stock)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoteDto {
    id: String,
    sku: String,
    cantidad_inicial: Decimal,
    disponible: Decimal,
    horneado_unix: i64,
    caduce_unix: i64,
}

#[tauri::command]
fn listar_lotes(estado: tauri::State<AppState>) -> Result<Vec<LoteDto>, UIError> {
    config_requerida(&estado)?;
    let lotes = con_ledger(&estado, |db| db.listar_lotes())?;
    Ok(lotes
        .into_iter()
        .map(|l| LoteDto {
            id: l.id,
            sku: l.sku,
            cantidad_inicial: l.cantidad_inicial,
            disponible: l.disponible,
            horneado_unix: l.horneado_unix,
            caduce_unix: l.caduce_unix,
        })
        .collect())
}

#[tauri::command]
fn crear_lote(
    estado: tauri::State<AppState>,
    sku: String,
    cantidad: String,
    caduce_unix: i64,
) -> Result<String, UIError> {
    let cfg = config_requerida(&estado)?;
    if capacidades_de_rubros(cfg.rubros) & capacidades::CAP_PERECEDERO == 0 {
        return Err(UIError::from(ErrorNegocio::CapacidadInactiva(
            capacidades::CAP_PERECEDERO,
        )));
    }
    let cant = decimal_de(&cantidad)?;
    if cant <= Decimal::ZERO || caduce_unix <= ahora_unix() {
        return Err(UIError::new(
            "lote invalido",
            "Cantidad mayor a cero y caducidad futura requeridas",
        ));
    }
    let sku_norm = sku.trim().to_uppercase();
    let lote = Lote {
        id: Uuid::new_v4().to_string(),
        sku: sku_norm.clone(),
        horneado_unix: ahora_unix(),
        cantidad_inicial: cant,
        disponible: cant,
        caduce_unix,
    };
    con_ledger(&estado, |db| db.guardar_lote(&lote))?;

    let mov = movimiento(&sku_norm, cant, MotivoMovimiento::Compra, None);
    con_ledger(&estado, |db| db.aplicar_movimiento(mov, |_, _| {}))?;
    Ok(lote.id)
}

/// Resuelve el precio efectivo de venta y las unidades base a descontar del stock
/// para el modo/presentacion solicitado. Compatible con presentaciones multiples y
/// con el campo legacy es_caja/nombre_paquete.
fn resolver_precio_presentacion(catalogo: &Catalogo, idx: usize, modo: &str) -> (Decimal, u32) {
    if modo == "unidad" || modo.is_empty() {
        return (catalogo.precio_usd(idx), 1);
    }
    // Buscar en presentaciones multiples
    if let Some((precio, upc)) = catalogo.presentacion_por_nombre(idx, modo) {
        return (precio, upc.max(1));
    }
    // Compatibilidad legacy: modo "paquete" con es_caja
    if modo == "paquete" && catalogo.es_caja(idx) {
        let upc = catalogo.unidades_por_caja(idx).unwrap_or(1).max(1);
        let precio = catalogo
            .precio_paquete_usd(idx)
            .unwrap_or_else(|| catalogo.precio_usd(idx) * Decimal::from(upc));
        return (precio, upc);
    }
    // Fallback: unidad base
    (catalogo.precio_usd(idx), 1)
}

/// Construye las lineas exactas de una venta (precio, factor y validaciones).
/// Es la unica fuente usada tanto por la previsualizacion como por el cobro:
/// lo que se muestra es exactamente lo que se graba.
fn construir_lineas_venta(
    catalogo: &Catalogo,
    items: &[ItemVentaDto],
    tasa: Decimal,
) -> Result<(LineasVenta, Vec<(usize, Decimal, String)>), UIError> {
    let mut lineas = LineasVenta::nuevas();
    let mut toques: Vec<(usize, Decimal, String)> = Vec::with_capacity(items.len());
    for item in items {
        let idx = catalogo
            .indice_de(item.sku.trim().to_uppercase().as_str())
            .ok_or(ErrorNegocio::ProductoInexistente)?;
        let cantidad = decimal_de(&item.cantidad)?;
        let modo = item.modo_venta.as_deref().unwrap_or("unidad");
        let (precio_efectivo, unidades_por_venta) =
            resolver_precio_presentacion(catalogo, idx, modo);
        let cant_unidades = cantidad * Decimal::from(unidades_por_venta);
        validar_linea(
            catalogo.capacidades(idx),
            cant_unidades,
            catalogo.stock(idx),
        )?;
        toques.push((idx, cantidad, modo.to_string()));
        lineas.agregar(
            catalogo.sku_obj(idx),
            catalogo.nombre_obj(idx),
            cantidad,
            precio_efectivo,
            tasa,
            modo.to_string(),
        );
    }
    Ok((lineas, toques))
}

fn parse_tasa_opcional(tasa_cobro: Option<&str>) -> Option<Decimal> {
    tasa_cobro
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| decimal_de(s).ok())
        .filter(|t| *t > Decimal::ZERO)
}

/// Registra el excedente retenido ("dejen eso asi") como ingreso auditable.
/// Solo aplica cuando la resolucion del vuelto fue RETENIDO.
fn registrar_redondeo_si_aplica(
    db: &Ledger,
    venta_id: &str,
    vuelto_bs: Decimal,
    tasa: Decimal,
    estado_vuelto: Option<&str>,
    metodo: Option<String>,
) -> Result<(), DbError> {
    if estado_vuelto != Some("RETENIDO") || vuelto_bs <= Decimal::ZERO {
        return Ok(());
    }
    let (jornada_id, operador) = match db.jornada_actual()? {
        Some(j) => (
            Some(j.id.clone()),
            if !j.operadores_activos.is_empty() {
                Some(j.operadores_activos.join(", "))
            } else if !j.operador_actual.is_empty() {
                Some(j.operador_actual.clone())
            } else {
                None
            },
        ),
        None => (None, None),
    };
    db.guardar_redondeo(&Redondeo {
        id: Uuid::new_v4().to_string(),
        venta_id: venta_id.to_string(),
        monto_bs: vuelto_bs,
        monto_usd: if tasa > Decimal::ZERO {
            Some(vuelto_bs / tasa)
        } else {
            None
        },
        tasa,
        fecha_unix: ahora_unix(),
        jornada_id,
        operador,
        metodo,
        firma_sha256: String::new(),
    })
}

/// Convierte los pagos del contrato a modelo persistido con validacion estricta:
/// un monto invalido o negativo no se puede convertir en cero en silencio.
fn pagos_desde_dto(pagos: Option<Vec<PagoTicketDto>>) -> Result<Vec<PagoVenta>, UIError> {
    let mut salida = Vec::new();
    for p in pagos.unwrap_or_default() {
        let monto_usd = decimal_de(&p.monto_usd)?;
        let monto_bs = decimal_de(&p.monto_bs)?;
        if monto_usd < Decimal::ZERO || monto_bs < Decimal::ZERO {
            return Err(UIError::new(
                "pago invalido",
                format!("El pago {} tiene un monto negativo", p.metodo),
            ));
        }
        let tasa_cambio = match p
            .tasa_cambio
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(t) => Some(decimal_de(t)?),
            None => None,
        };
        salida.push(PagoVenta {
            metodo: p.metodo,
            moneda: p.moneda.unwrap_or_else(|| "BS".to_string()),
            monto_usd,
            monto_bs,
            tasa_cambio,
            referencia: p.referencia,
        });
    }
    Ok(salida)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TotalesCobroDto {
    total_usd: Decimal,
    total_bs: Decimal,
    tasa_aplicada: Decimal,
}

#[tauri::command]
fn previsualizar_cobro(
    estado: tauri::State<AppState>,
    items: Vec<ItemVentaDto>,
    tasa_cobro: Option<String>,
) -> Result<TotalesCobroDto, UIError> {
    if items.is_empty() {
        return Err(UIError::new("venta vacia", "Agregue productos al carrito"));
    }
    config_requerida(&estado)?;
    let tasa = parse_tasa_opcional(tasa_cobro.as_deref()).unwrap_or(tasa_viva(&estado)?);
    con_ledger(&estado, |db| -> Result<TotalesCobroDto, UIError> {
        let catalogo = catalogo_fresco(db)?;
        let (lineas, _) = construir_lineas_venta(&catalogo, &items, tasa)?;
        Ok(TotalesCobroDto {
            total_usd: lineas.total_usd(),
            total_bs: lineas.total_bs(),
            tasa_aplicada: tasa,
        })
    })
}

// ---------------- comandos: venta directa ----------------

#[tauri::command]
fn registrar_venta(
    estado: tauri::State<AppState>,
    items: Vec<ItemVentaDto>,
    monto_recibido_bs: String,
    tasa_cobro: Option<String>,
    pagos: Option<Vec<PagoTicketDto>>,
    resolucion_vuelto: Option<ResolucionVueltoDto>,
    idempotency_key: Option<String>,
) -> Result<TicketDto, UIError> {
    if items.is_empty() {
        return Err(UIError::new("venta vacia", "Agregue productos al carrito"));
    }
    let _cfg = config_requerida(&estado)?;
    jornada_requerida(&estado)?;
    // La tasa que muestra la pantalla manda: el backend graba exactamente esa.
    let tasa = parse_tasa_opcional(tasa_cobro.as_deref()).unwrap_or(tasa_viva(&estado)?);
    let recibido = decimal_de(&monto_recibido_bs)?;

    // Idempotencia atomica: el guard vive durante toda la venta para que dos
    // invocaciones con la misma clave no creen dos ventas.
    let mut dedup_guard = match idempotency_key.as_ref() {
        Some(_) => Some(estado.dedup_ventas.lock().map_err(|_| {
            UIError::new(
                "idempotencia bloqueada",
                "El mutex de idempotencia envenenado",
            )
        })?),
        None => None,
    };
    if let (Some(key), Some(dedup)) = (idempotency_key.as_ref(), dedup_guard.as_mut()) {
        let now = ahora_unix();
        dedup.retain(|_, (ts, _)| now - *ts < 300);
        if let Some((_, venta_id)) = dedup.get(key) {
            // Replay: se devuelve el ticket original, con sus montos originales.
            let venta_id_clone = venta_id.clone();
            return con_ledger(&estado, |db| -> Result<TicketDto, DbError> {
                let venta = db
                    .cargar_venta(&venta_id_clone)?
                    .ok_or(DbError::Negocio(ErrorNegocio::ProductoInexistente))?;
                Ok(armar_ticket(
                    &venta,
                    venta.monto_recibido_bs,
                    venta.vuelto_bs,
                ))
            });
        }
    }

    let resultado = con_ledger(&estado, |db| {
        let catalogo = catalogo_fresco(db)?;
        let (lineas, toques) = construir_lineas_venta(&catalogo, &items, tasa)?;

        let total_usd = lineas.total_usd();
        let total_bs = lineas.total_bs();
        // Use epsilon tolerance for comparison to handle floating point precision issues
        let epsilon = Decimal::from_str("0.01").unwrap_or(Decimal::ZERO);
        if recibido > Decimal::ZERO && (total_bs - recibido) > epsilon {
            return Err(UIError::new(
                "monto insuficiente",
                "El pago recibido no cubre el total en bolivares",
            ));
        }
        let vuelto = if recibido > Decimal::ZERO {
            recibido - total_bs
        } else {
            Decimal::ZERO
        };

        let pagos_model: Vec<PagoVenta> = pagos_desde_dto(pagos)?;

        let (estado_v, metodo_v, monto_v_usd, tasa_v) = match resolucion_vuelto {
            Some(res) => (
                Some(res.estado),
                res.metodo,
                res.monto_usd.and_then(|m| decimal_de(&m).ok()),
                res.tasa.and_then(|t| decimal_de(&t).ok()),
            ),
            None => {
                let est = if vuelto > Decimal::ZERO {
                    Some("PAGADO".to_string())
                } else {
                    Some("SIN_VUELTO".to_string())
                };
                (est, None, None, None)
            }
        };

        let venta_id = Uuid::new_v4().to_string();
        for (idx, cantidad, modo) in &toques {
            descontar_con_lotes_interno(db, &catalogo, *idx, *cantidad, &venta_id, modo)?;
        }

        // Capturar operador activo de la jornada para trazabilidad del ticket
        let operador_activo: Option<String> = db.jornada_actual().ok().flatten().map(|j| {
            if !j.operadores_activos.is_empty() {
                j.operadores_activos.join(", ")
            } else {
                j.operador_actual.clone()
            }
        });

        let venta = Venta {
            id: venta_id,
            etiqueta: String::new(),
            es_cuenta_abierta: false,
            estado: EstadoVenta::Cerrada,
            lineas,
            tasa_del_dia: tasa,
            total_usd,
            total_bs,
            monto_recibido_bs: recibido,
            vuelto_bs: vuelto,
            pagos: pagos_model,
            estado_vuelto: estado_v,
            metodo_vuelto: metodo_v,
            monto_vuelto_usd: monto_v_usd,
            tasa_vuelto: tasa_v,
            fecha_apertura_unix: ahora_unix(),
            fecha_cierre_unix: ahora_unix(),
            firma_sha256: String::new(),
            tipo: "venta".to_string(),
            cliente: operador_activo,
            nota: None,
            abonos_usd: None,
            abonos_bs: None,
        };
        let firmada = db.guardar_venta(venta)?;
        registrar_redondeo_si_aplica(
            db,
            &firmada.id,
            firmada.vuelto_bs,
            firmada.tasa_del_dia,
            firmada.estado_vuelto.as_deref(),
            firmada.metodo_vuelto.clone(),
        )?;

        // Actualizar contadores de la jornada activa
        if let Ok(Some(mut jornada)) = db.jornada_actual() {
            jornada.ventas_total_usd += total_usd;
            jornada.ventas_total_bs += total_bs;
            jornada.tickets_emitidos += 1;
            match firmada.estado_vuelto.as_deref() {
                Some("PAGADO") => {
                    jornada.vuelto_pagado_bs += vuelto;
                }
                Some("RETENIDO") => {
                    jornada.vuelto_retenido_bs += vuelto;
                }
                _ => {}
            }
            db.guardar_jornada(&jornada)?;
        }

        Ok(armar_ticket(&firmada, recibido, vuelto))
    });

    // Registrar la clave solo tras una venta exitosa (el guard sigue vivo).
    if let (Some(key), Ok(ticket), Some(dedup)) =
        (&idempotency_key, &resultado, dedup_guard.as_mut())
    {
        dedup.insert(key.clone(), (ahora_unix(), ticket.venta_id.clone()));
    }

    resultado
}

fn armar_ticket(venta: &Venta, recibido: Decimal, vuelto: Decimal) -> TicketDto {
    let pagos_dto: Option<Vec<PagoTicketDto>> = if venta.pagos.is_empty() {
        None
    } else {
        Some(
            venta
                .pagos
                .iter()
                .map(|p| PagoTicketDto {
                    metodo: p.metodo.clone(),
                    moneda: Some(p.moneda.clone()),
                    monto_usd: p.monto_usd.to_string(),
                    monto_bs: p.monto_bs.to_string(),
                    tasa_cambio: p.tasa_cambio.map(|t| t.to_string()),
                    referencia: p.referencia.clone(),
                })
                .collect(),
        )
    };

    TicketDto {
        venta_id: venta.id.clone(),
        total_usd: venta.total_usd,
        total_bs: venta.total_bs,
        monto_recibido_bs: recibido,
        vuelto_bs: vuelto,
        tasa_del_dia: venta.tasa_del_dia,
        pagos: pagos_dto,
        estado_vuelto: venta.estado_vuelto.clone(),
        metodo_vuelto: venta.metodo_vuelto.clone(),
        monto_vuelto_bs: if vuelto > Decimal::ZERO {
            Some(vuelto.to_string())
        } else {
            None
        },
        monto_vuelto_usd: venta.monto_vuelto_usd.map(|m| m.to_string()),
        tasa_vuelto: venta.tasa_vuelto.map(|t| t.to_string()),
        fecha_hora: chrono::DateTime::from_timestamp(venta.fecha_cierre_unix, 0).map(|dt| {
            let tz_vet = chrono::FixedOffset::east_opt(-4 * 3600).unwrap();
            dt.with_timezone(&tz_vet)
                .format("%d/%m/%Y %H:%M")
                .to_string()
        }),
        canal: Some(
            if venta.tipo == "deuda" && venta.estado == EstadoVenta::Cerrada {
                "LIQUIDACION DE DEUDA".to_string()
            } else if venta.es_cuenta_abierta {
                "CONSUMO EN CUENTA".to_string()
            } else if venta.tipo == "saldo_a_favor" {
                "SALDO A FAVOR".to_string()
            } else {
                "VENTA DIRECTA".to_string()
            },
        ),
        operador: venta.cliente.clone(),
        etiqueta: {
            let e = venta.etiqueta.trim();
            if e.is_empty() {
                None
            } else {
                Some(e.to_string())
            }
        },
        fecha_unix: venta.fecha_cierre_unix,
        lineas: (0..venta.lineas.skus.len())
            .map(|i| {
                let sub_usd = venta.lineas.cantidades[i] * venta.lineas.precios_usd[i];
                LineaTicketDto {
                    sku: venta.lineas.skus[i].clone(),
                    nombre: venta.lineas.nombres[i].clone(),
                    cantidad: venta.lineas.cantidades[i],
                    precio_usd: venta.lineas.precios_usd[i],
                    tasa_bloqueada: venta.lineas.tasas_bloqueadas[i],
                    subtotal_usd: sub_usd,
                    subtotal_bs: sub_usd * venta.lineas.tasas_bloqueadas[i],
                    modo_venta: venta.lineas.modos_venta.get(i).cloned(),
                }
            })
            .collect(),
    }
}

// ---------------- comandos: cuenta abierta (licoreria) ----------------

#[tauri::command]
fn abrir_cuenta(
    estado: tauri::State<AppState>,
    etiqueta: String,
    tipo: Option<String>,
    cliente: Option<String>,
    nota: Option<String>,
) -> Result<CuentaDto, UIError> {
    let cfg = config_requerida(&estado)?;
    jornada_requerida(&estado)?;
    let permitidas = capacidades_de_rubros(cfg.rubros);
    if !licoreria::admite_cuenta_abierta(permitidas) {
        return Err(UIError::from(ErrorNegocio::CapacidadInactiva(
            capacidades::CAP_CUENTA_ABIERTA,
        )));
    }
    if etiqueta.trim().is_empty() {
        return Err(UIError::new(
            "etiqueta requerida",
            "Identifique mesa o cliente",
        ));
    }
    let venta = Venta {
        id: Uuid::new_v4().to_string(),
        etiqueta: etiqueta.trim().to_string(),
        es_cuenta_abierta: true,
        estado: EstadoVenta::Abierta,
        lineas: LineasVenta::nuevas(),
        tasa_del_dia: Decimal::ZERO,
        total_usd: Decimal::ZERO,
        total_bs: Decimal::ZERO,
        monto_recibido_bs: Decimal::ZERO,
        vuelto_bs: Decimal::ZERO,
        pagos: Vec::new(),
        estado_vuelto: None,
        metodo_vuelto: None,
        monto_vuelto_usd: None,
        tasa_vuelto: None,
        fecha_apertura_unix: ahora_unix(),
        fecha_cierre_unix: 0,
        firma_sha256: String::new(),
        tipo: tipo.unwrap_or_else(|| "activa".to_string()),
        cliente,
        nota,
        abonos_usd: Some(Decimal::ZERO),
        abonos_bs: Some(Decimal::ZERO),
    };
    let guardada = con_ledger(&estado, |db| db.guardar_venta(venta))?;
    Ok(cuenta_dto(&guardada))
}

fn cuenta_dto(v: &Venta) -> CuentaDto {
    let consumos: Vec<ConsumoCuentaDto> = (0..v.lineas.skus.len())
        .map(|i| ConsumoCuentaDto {
            id: format!("consumo-{}", i),
            sku: v.lineas.skus[i].as_str().to_string(),
            nombre: v.lineas.nombres[i].as_str().to_string(),
            cantidad: v.lineas.cantidades[i],
            precio_usd: v.lineas.precios_usd[i],
            subtotal_usd: v.lineas.cantidades[i] * v.lineas.precios_usd[i],
            modo_venta: v.lineas.modos_venta.get(i).cloned(),
        })
        .collect();
    CuentaDto {
        venta_id: v.id.clone(),
        etiqueta: v.etiqueta.clone(),
        lineas: v.lineas.skus.len(),
        total_parcial_usd: v.lineas.total_usd(),
        total_parcial_bs: v.lineas.total_bs(),
        abonos_usd: v.abonos_usd,
        abonos_bs: v.abonos_bs,
        tipo: Some(v.tipo.clone()),
        cliente: v.cliente.clone(),
        nota: v.nota.clone(),
        consumos,
        fecha_apertura_unix: v.fecha_apertura_unix,
    }
}

#[tauri::command]
fn listar_cuentas(estado: tauri::State<AppState>) -> Result<Vec<CuentaDto>, UIError> {
    config_requerida(&estado)?;
    Ok(con_ledger(&estado, |db| db.cuentas_abiertas())?
        .iter()
        .map(cuenta_dto)
        .collect())
}

#[tauri::command]
fn agregar_consumo(
    estado: tauri::State<AppState>,
    venta_id: String,
    sku: String,
    cantidad: String,
    modo_venta: Option<String>,
) -> Result<CuentaDto, UIError> {
    let _cfg = config_requerida(&estado)?;
    jornada_requerida(&estado)?;
    let tasa = tasa_viva(&estado)?;
    let cant = decimal_de(&cantidad)?;
    let modo = modo_venta.unwrap_or_else(|| "unidad".to_string());

    let actualizada = con_ledger(&estado, |db| {
        let mut venta = db
            .cargar_venta(&venta_id)?
            .filter(|v| v.es_cuenta_abierta && v.estado == EstadoVenta::Abierta)
            .ok_or(DbError::Negocio(ErrorNegocio::CuentaInvalida(
                venta_id.clone(),
            )))?;

        let catalogo = catalogo_fresco(db)?;
        let idx = catalogo
            .indice_de(sku.trim().to_uppercase().as_str())
            .ok_or(DbError::Negocio(ErrorNegocio::ProductoInexistente))?;
        let (precio_efectivo, unidades_por_venta) =
            resolver_precio_presentacion(&catalogo, idx, &modo);
        let cant_unidades = cant * Decimal::from(unidades_por_venta);
        validar_linea(
            catalogo.capacidades(idx),
            cant_unidades,
            catalogo.stock(idx),
        )?;

        let sku_obj = catalogo.sku_obj(idx);
        let modo_str = modo.as_str();
        let pos_existente = venta.lineas.skus.iter().enumerate().position(|(i, s)| {
            s == &sku_obj
                && venta
                    .lineas
                    .modos_venta
                    .get(i)
                    .map(|m| m.as_str())
                    .unwrap_or("unidad")
                    == modo_str
        });
        if let Some(pos) = pos_existente {
            venta.lineas.cantidades[pos] += cant;
        } else {
            venta.lineas.agregar(
                sku_obj,
                catalogo.nombre_obj(idx),
                cant,
                precio_efectivo,
                tasa,
                modo.clone(),
            );
        }
        descontar_con_lotes_interno(db, &catalogo, idx, cant, &venta.id, &modo)?;
        db.guardar_venta(venta)
    })?;
    Ok(cuenta_dto(&actualizada))
}

#[tauri::command]
fn previsualizar_cierre_cuenta(
    estado: tauri::State<AppState>,
    venta_id: String,
    tasa_cobro: Option<String>,
) -> Result<TotalesCobroDto, UIError> {
    config_requerida(&estado)?;
    let tasa = parse_tasa_opcional(tasa_cobro.as_deref()).unwrap_or(tasa_viva(&estado)?);
    let id = venta_id.clone();
    con_ledger(&estado, |db| -> Result<TotalesCobroDto, UIError> {
        let venta = db
            .cargar_venta(&id)?
            .filter(|v| v.es_cuenta_abierta && v.estado == EstadoVenta::Abierta)
            .ok_or(DbError::Negocio(ErrorNegocio::CuentaInvalida(venta_id)))?;
        let cierre = licoreria::liquidar_cierre(&venta.lineas);
        let abonos_usd = venta
            .abonos_usd
            .filter(|v| *v > Decimal::ZERO)
            .unwrap_or(Decimal::ZERO);
        let abonos_bs = venta
            .abonos_bs
            .filter(|v| *v > Decimal::ZERO)
            .unwrap_or(Decimal::ZERO);
        let total_neto_usd = if abonos_usd > cierre.total_usd {
            Decimal::ZERO
        } else {
            cierre.total_usd - abonos_usd
        };
        let bruto_bs = total_neto_usd * tasa;
        let total_neto_bs = if abonos_bs > bruto_bs {
            Decimal::ZERO
        } else {
            bruto_bs - abonos_bs
        };
        Ok(TotalesCobroDto {
            total_usd: total_neto_usd,
            total_bs: total_neto_bs,
            tasa_aplicada: tasa,
        })
    })
}

#[tauri::command]
fn cerrar_cuenta(
    estado: tauri::State<AppState>,
    venta_id: String,
    monto_recibido_bs: String,
    tasa_cobro: Option<String>,
    pagos: Option<Vec<PagoTicketDto>>,
    resolucion_vuelto: Option<ResolucionVueltoDto>,
) -> Result<TicketDto, UIError> {
    let _cfg = config_requerida(&estado)?;
    jornada_requerida(&estado)?;
    let recibido = decimal_de(&monto_recibido_bs)?;
    let tasa_servicio = estado.servicio_tasa.info_actual().valor;
    let tasa = match tasa_cobro
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(t_str) => match decimal_de(t_str) {
            Ok(t) if t > Decimal::ZERO => t,
            _ => tasa_servicio,
        },
        None => tasa_servicio,
    };

    // Capturar el operador activo de la jornada antes del closure de DB
    let operador_cierre: Option<String> = con_ledger(&estado, |db| {
        Ok::<Option<String>, DbError>(db.jornada_actual().ok().flatten().map(|j| {
            if !j.operadores_activos.is_empty() {
                j.operadores_activos.join(", ")
            } else {
                j.operador_actual.clone()
            }
        }))
    })
    .ok()
    .flatten();

    let cerrada = con_ledger(&estado, |db| {
        let mut venta = db
            .cargar_venta(&venta_id)?
            .filter(|v| v.es_cuenta_abierta && v.estado == EstadoVenta::Abierta)
            .ok_or(DbError::Negocio(ErrorNegocio::CuentaInvalida(venta_id)))?;

        let cierre = licoreria::liquidar_cierre(&venta.lineas);
        let abonos_usd = venta
            .abonos_usd
            .filter(|v| *v > Decimal::ZERO)
            .unwrap_or(Decimal::ZERO);
        let abonos_bs = venta
            .abonos_bs
            .filter(|v| *v > Decimal::ZERO)
            .unwrap_or(Decimal::ZERO);

        let total_neto_usd = if abonos_usd > cierre.total_usd {
            Decimal::ZERO
        } else {
            cierre.total_usd - abonos_usd
        };
        // El abono en bolivares reduce el saldo en bolivares de forma exacta.
        let bruto_bs = total_neto_usd * tasa;
        let total_neto_bs = if abonos_bs > bruto_bs {
            Decimal::ZERO
        } else {
            bruto_bs - abonos_bs
        };

        // Use epsilon tolerance for comparison to handle floating point precision issues
        let epsilon = Decimal::from_str("0.01").unwrap_or(Decimal::ZERO);
        if recibido > Decimal::ZERO && (total_neto_bs - recibido) > epsilon {
            return Err(DbError::Negocio(ErrorNegocio::PagoInsuficiente {
                requerido: total_neto_bs,
                recibido,
            }));
        }

        let vuelto = if recibido > Decimal::ZERO {
            recibido - total_neto_bs
        } else {
            Decimal::ZERO
        };

        let pagos_model: Vec<PagoVenta> = pagos_desde_dto(pagos).map_err(|e| {
            DbError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                e.message,
            ))
        })?;

        let (estado_v, metodo_v, monto_v_usd, tasa_v) = match resolucion_vuelto {
            Some(res) => (
                Some(res.estado),
                res.metodo,
                res.monto_usd.and_then(|m| decimal_de(&m).ok()),
                res.tasa.and_then(|t| decimal_de(&t).ok()),
            ),
            None => {
                let est = if vuelto > Decimal::ZERO {
                    Some("PAGADO".to_string())
                } else {
                    Some("SIN_VUELTO".to_string())
                };
                (est, None, None, None)
            }
        };

        venta.estado = EstadoVenta::Cerrada;
        // La tasa auditada del cierre es la que liquida el saldo; sin esto el
        // ticket de una cuenta/deuda liquidada mostraba tasa 0,00.
        venta.tasa_del_dia = tasa;
        venta.total_usd = total_neto_usd;
        venta.total_bs = total_neto_bs;
        venta.monto_recibido_bs = recibido;
        venta.vuelto_bs = vuelto;
        venta.pagos = pagos_model;
        venta.estado_vuelto = estado_v;
        venta.metodo_vuelto = metodo_v;
        venta.monto_vuelto_usd = monto_v_usd;
        venta.tasa_vuelto = tasa_v;
        venta.fecha_cierre_unix = ahora_unix();
        // El operador del ticket es siempre el activo en la jornada al momento del cierre.
        // Se sobreescribe venta.cliente con operador_cierre para que armar_ticket lo use
        // correctamente. El campo cliente de la cuenta (etiqueta de mesa o nombre de deudor)
        // no debe propagarse como operador del ticket.
        if let Some(ref op) = operador_cierre {
            venta.cliente = Some(op.clone());
        }
        let cerrada = db.guardar_venta(venta)?;
        registrar_redondeo_si_aplica(
            db,
            &cerrada.id,
            cerrada.vuelto_bs,
            cerrada.tasa_del_dia,
            cerrada.estado_vuelto.as_deref(),
            cerrada.metodo_vuelto.clone(),
        )?;

        if abonos_usd > cierre.total_usd {
            let excedente = abonos_usd - cierre.total_usd;
            let saldo_venta = Venta {
                id: Uuid::new_v4().to_string(),
                etiqueta: format!("Saldo a favor de {}", cerrada.etiqueta),
                es_cuenta_abierta: false,
                estado: EstadoVenta::Cerrada,
                lineas: LineasVenta::nuevas(),
                tasa_del_dia: tasa,
                total_usd: Decimal::ZERO,
                total_bs: Decimal::ZERO,
                monto_recibido_bs: Decimal::ZERO,
                vuelto_bs: Decimal::ZERO,
                pagos: vec![PagoVenta {
                    metodo: "SALDO.A_FAVOR".to_string(),
                    moneda: "USD".to_string(),
                    monto_usd: excedente,
                    monto_bs: excedente * tasa,
                    tasa_cambio: Some(tasa),
                    referencia: Some(cerrada.id.clone()),
                }],
                estado_vuelto: Some("SALDO_A_FAVOR".to_string()),
                metodo_vuelto: None,
                monto_vuelto_usd: Some(excedente),
                tasa_vuelto: Some(tasa),
                fecha_apertura_unix: ahora_unix(),
                fecha_cierre_unix: ahora_unix(),
                firma_sha256: String::new(),
                tipo: "saldo_a_favor".to_string(),
                cliente: cerrada.cliente.clone(),
                nota: Some(format!(
                    "Saldo a favor de ${:.2} generado al cerrar {}",
                    excedente, cerrada.id
                )),
                abonos_usd: None,
                abonos_bs: None,
            };
            db.guardar_venta(saldo_venta)?;
        }

        // Actualizar contadores de la jornada activa
        if let Ok(Some(mut jornada)) = db.jornada_actual() {
            jornada.ventas_total_usd += total_neto_usd;
            jornada.ventas_total_bs += total_neto_bs;
            jornada.tickets_emitidos += 1;
            if cerrada.tipo == "deuda" {
                jornada.deudas_liquidadas_usd += total_neto_usd;
            }
            match cerrada.estado_vuelto.as_deref() {
                Some("PAGADO") => {
                    jornada.vuelto_pagado_bs += vuelto;
                }
                Some("RETENIDO") => {
                    jornada.vuelto_retenido_bs += vuelto;
                }
                _ => {}
            }
            db.guardar_jornada(&jornada)?;
        }

        Ok(cerrada)
    })?;

    Ok(armar_ticket(
        &cerrada,
        cerrada.monto_recibido_bs,
        cerrada.vuelto_bs,
    ))
}

// ---------------- comandos: panel del dueno ----------------

#[tauri::command]
fn datos_panel(estado: tauri::State<AppState>) -> Result<PanelDto, UIError> {
    config_requerida(&estado)?;
    let tasa = tasa_viva(&estado)?;
    con_ledger(&estado, |db| -> Result<PanelDto, DbError> {
        let catalogo = db.cargar_catalogo()?;
        let limite = ahora_unix() - 86_400;
        let ventas = db.ventas_recientes(2_000)?;

        let mut usd = Decimal::ZERO;
        let mut bs = Decimal::ZERO;
        let mut tickets = 0usize;

        const MAX_VENDIDOS: usize = 32;
        const MAX_TOP: usize = 5;
        let mut vendidos_nombre: [Nombre; MAX_VENDIDOS] = [Nombre::empty(); MAX_VENDIDOS];
        let mut vendidos_cant: [Decimal; MAX_VENDIDOS] = [Decimal::ZERO; MAX_VENDIDOS];
        let mut vendidos_len: usize = 0;

        for v in &ventas {
            if v.estado != EstadoVenta::Cerrada || v.fecha_cierre_unix < limite {
                continue;
            }
            tickets += 1;
            usd += v.total_usd;
            bs += v.total_bs;
            for i in 0..v.lineas.nombres.len() {
                let nombre = &v.lineas.nombres[i];
                let cant = v.lineas.cantidades[i];
                let mut found = false;
                for j in 0..vendidos_len {
                    if vendidos_nombre[j].as_str() == nombre.as_str() {
                        vendidos_cant[j] += cant;
                        found = true;
                        break;
                    }
                }
                if !found && vendidos_len < MAX_VENDIDOS {
                    vendidos_nombre[vendidos_len] = nombre.clone();
                    vendidos_cant[vendidos_len] = cant;
                    vendidos_len += 1;
                }
            }
        }

        let mut top_len: usize = 0;
        let mut top_nombre: [Nombre; MAX_TOP] = [Nombre::empty(); MAX_TOP];
        let mut top_cant: [Decimal; MAX_TOP] = [Decimal::ZERO; MAX_TOP];
        for k in 0..vendidos_len {
            if top_len < MAX_TOP {
                top_nombre[top_len] = vendidos_nombre[k].clone();
                top_cant[top_len] = vendidos_cant[k];
                top_len += 1;
                let mut m = top_len - 1;
                while m > 0 && top_cant[m] > top_cant[m - 1] {
                    top_nombre.swap(m, m - 1);
                    top_cant.swap(m, m - 1);
                    m -= 1;
                }
            } else if vendidos_cant[k] > top_cant[MAX_TOP - 1] {
                top_nombre[MAX_TOP - 1] = vendidos_nombre[k].clone();
                top_cant[MAX_TOP - 1] = vendidos_cant[k];
                let mut m = MAX_TOP - 1;
                while m > 0 && top_cant[m] > top_cant[m - 1] {
                    top_nombre.swap(m, m - 1);
                    top_cant.swap(m, m - 1);
                    m -= 1;
                }
            }
        }

        let mut top: Vec<TopProductoDto> = Vec::with_capacity(top_len);
        for i in 0..top_len {
            top.push(TopProductoDto {
                nombre: top_nombre[i].clone(),
                cantidad: top_cant[i],
            });
        }

        let mut criticos: Vec<CriticoDto> = Vec::new();
        for i in 0..catalogo.len() {
            if catalogo.stock(i) <= dec!(5) && criticos.len() < 16 {
                criticos.push(CriticoDto {
                    sku: catalogo.sku_obj(i),
                    nombre: catalogo.nombre_obj(i),
                    stock: catalogo.stock(i),
                });
            }
        }

        let cuentas_todas = db.cuentas_abiertas()?;
        let abiertas = cuentas_todas.len();

        // Financial metrics - calculate from actual product costs
        let mut total_venta_usd = Decimal::ZERO;
        let mut total_costo_usd = Decimal::ZERO;
        for i in 0..catalogo.len() {
            let precio_venta = catalogo.precio_usd(i);
            let precio_bruto = catalogo.precio_bruto_usd(i).unwrap_or(Decimal::ZERO);
            if precio_venta > Decimal::ZERO {
                total_venta_usd += precio_venta;
                total_costo_usd += if precio_bruto > Decimal::ZERO {
                    precio_bruto
                } else {
                    precio_venta * dec!(0.65)
                };
            }
        }
        let costo_total_usd = if total_venta_usd > Decimal::ZERO {
            usd * total_costo_usd / total_venta_usd
        } else {
            usd * dec!(0.65)
        };
        let ganancia_bruta_usd = usd - costo_total_usd;
        let impuestos_usd = usd * dec!(0.12);
        let ganancia_neta_usd = ganancia_bruta_usd - impuestos_usd;
        let ganancia_neta_sin_imp_usd = ganancia_bruta_usd;
        let ganancia_neta_bs = ganancia_neta_usd * tasa;

        // Deudas y dinero en la calle
        let deudas_abiertas = cuentas_todas.iter().filter(|v| v.tipo == "deuda").count();
        let dinero_en_la_calle_usd: Decimal = cuentas_todas
            .iter()
            .filter(|v| v.tipo == "deuda")
            .map(|v| {
                let total = v.lineas.total_usd();
                let abonos = v.abonos_usd.unwrap_or(Decimal::ZERO);
                if total > abonos {
                    total - abonos
                } else {
                    Decimal::ZERO
                }
            })
            .sum();
        let dinero_en_la_calle_bs = dinero_en_la_calle_usd * tasa;

        // Dinero por categoria
        let categorias = db.listar_categorias()?;
        let mut mapa_cats: std::collections::HashMap<String, (usize, Decimal, Decimal, Decimal)> =
            std::collections::HashMap::new();
        let mut total_bruto_global: Decimal = Decimal::ZERO;
        for i in 0..catalogo.len() {
            let cat_id = catalogo
                .categoria_id(i)
                .unwrap_or_else(|| "cat-general".to_string());
            let entry = mapa_cats.entry(cat_id.clone()).or_insert((
                0,
                Decimal::ZERO,
                Decimal::ZERO,
                Decimal::ZERO,
            ));
            entry.0 += 1;
            let st = if catalogo.sin_stock(i) {
                Decimal::ZERO
            } else {
                catalogo.stock(i)
            };
            let bruto_unit = catalogo
                .precio_bruto_usd(i)
                .unwrap_or(catalogo.precio_usd(i) * dec!(0.65));
            let venta_unit = catalogo.precio_usd(i);
            entry.1 += st;
            entry.2 += st * bruto_unit;
            entry.3 += st * venta_unit;
            total_bruto_global += st * bruto_unit;
        }
        let cat_nombre_map: std::collections::HashMap<String, String> = categorias
            .iter()
            .map(|c| (c.id.clone(), c.nombre.clone()))
            .collect();
        let mut dinero_por_categoria: Vec<CategoriaDineroBrutoDto> = mapa_cats
            .into_iter()
            .filter(|(_, (n, _, b, _))| *n > 0 || *b > Decimal::ZERO)
            .map(|(cat_id, (cant_prod, unidades, bruto, venta))| {
                let nombre = cat_nombre_map
                    .get(&cat_id)
                    .cloned()
                    .unwrap_or_else(|| "General".to_string());
                let margen = if venta > bruto {
                    venta - bruto
                } else {
                    Decimal::ZERO
                };
                let margen_pct = if venta > Decimal::ZERO {
                    format!("{:.1}", margen / venta * dec!(100))
                } else {
                    "0.0".to_string()
                };
                let pct_cap = if total_bruto_global > Decimal::ZERO {
                    format!("{:.1}", bruto / total_bruto_global * dec!(100))
                } else {
                    "0.0".to_string()
                };
                CategoriaDineroBrutoDto {
                    categoria_id: cat_id,
                    nombre,
                    cantidad_productos: cant_prod,
                    unidades_stock: unidades,
                    dinero_bruto_usd: bruto,
                    dinero_bruto_bs: bruto * tasa,
                    dinero_venta_usd: venta,
                    dinero_venta_bs: venta * tasa,
                    margen_bruto_proyectado_usd: margen,
                    margen_bruto_pct: margen_pct,
                    porcentaje_capital: pct_cap,
                }
            })
            .collect();
        dinero_por_categoria.sort_by(|a, b| b.dinero_bruto_usd.cmp(&a.dinero_bruto_usd));

        Ok(PanelDto {
            ventas_24h_usd: usd,
            ventas_24h_bs: bs,
            tickets_24h: tickets,
            valor_inventario_usd: catalogo.valor_inventario_usd(),
            total_productos: catalogo.len(),
            costo_total_usd,
            ganancia_bruta_usd,
            ganancia_neta_usd,
            ganancia_neta_sin_imp_usd,
            ganancia_neta_bs,
            criticos,
            cuentas_abiertas: abiertas,
            deudas_abiertas,
            dinero_en_la_calle_usd,
            dinero_en_la_calle_bs,
            top_productos: top,
            dinero_por_categoria,
        })
    })
}

#[tauri::command]
fn obtener_tasa_bcv(state: tauri::State<AppState>) -> Result<TasaInfo, UIError> {
    Ok(state.servicio_tasa.info_actual())
}

#[tauri::command]
async fn forzar_actualizacion_tasa(state: tauri::State<'_, AppState>) -> Result<TasaInfo, UIError> {
    let resultado = state.servicio_tasa.refrescar().await;
    resultado
}

#[tauri::command]
async fn volver_a_bcv(state: tauri::State<'_, AppState>) -> Result<TasaInfo, UIError> {
    let resultado = state.servicio_tasa.volver_a_bcv().await;
    resultado
}

#[tauri::command]
fn fijar_tasa_manual(state: tauri::State<AppState>, tasa: String) -> Result<TasaInfo, UIError> {
    let valor = Decimal::from_str(&tasa.trim().replace(',', "."))
        .map_err(|_| UIError::new("tasa invalida", "Formato decimal invalido"))?;
    let resultado = state.servicio_tasa.establecer_tasa_manual(valor);
    resultado
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupMetadataDto {
    version: u32,
    timestamp_unix: i64,
    arboles: Vec<String>,
    total_registros: usize,
    checksum_sha256: String,
}

#[derive(Deserialize)]
struct BackupRuta {
    ruta: String,
    #[serde(default)]
    licencia: Option<String>,
}

#[tauri::command]
fn exportar_backup(
    state: tauri::State<AppState>,
    payload: BackupRuta,
) -> Result<BackupMetadataDto, UIError> {
    let db = state
        .ledger
        .lock()
        .map_err(|_| UIError::new("db bloqueada", ""))?;
    let meta = db.exportar_backup(&payload.ruta)?;
    Ok(BackupMetadataDto {
        version: meta.version,
        timestamp_unix: meta.timestamp_unix,
        arboles: meta.arboles,
        total_registros: meta.total_registros,
        checksum_sha256: meta.checksum_sha256,
    })
}

#[tauri::command]
fn importar_backup(
    state: tauri::State<AppState>,
    payload: BackupRuta,
) -> Result<BackupMetadataDto, UIError> {
    let db = state
        .ledger
        .lock()
        .map_err(|_| UIError::new("db bloqueada", ""))?;
    let lic = payload.licencia.as_deref().unwrap_or("");
    let meta = db.importar_backup(&payload.ruta, lic)?;
    Ok(BackupMetadataDto {
        version: meta.version,
        timestamp_unix: meta.timestamp_unix,
        arboles: meta.arboles,
        total_registros: meta.total_registros,
        checksum_sha256: meta.checksum_sha256,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RespaldoInfo {
    id: String,
    fecha: String,
    archivo_nombre: String,
    registros: usize,
    tamano_kb: u64,
    checksum_sha256: String,
}

#[tauri::command]
fn listar_backups(
    state: tauri::State<AppState>,
    directorio: String,
) -> Result<Vec<RespaldoInfo>, UIError> {
    let db = state
        .ledger
        .lock()
        .map_err(|_| UIError::new("db bloqueada", ""))?;
    let metas = db.listar_backups(&directorio)?;
    let mut infos = Vec::new();
    for (archivo_nombre, meta) in metas {
        let ruta = std::path::Path::new(&directorio).join(&archivo_nombre);
        let id = archivo_nombre.replace(".backup", "");
        let fecha = fecha_vet_str(meta.timestamp_unix, "%Y-%m-%d %H:%M");
        let tamano_kb = std::fs::metadata(&ruta)
            .map(|m| m.len() / 1024)
            .unwrap_or(0);
        infos.push(RespaldoInfo {
            id,
            fecha,
            archivo_nombre,
            registros: meta.total_registros,
            tamano_kb,
            checksum_sha256: meta.checksum_sha256,
        });
    }
    Ok(infos)
}

#[tauri::command]
fn auto_backup(
    state: tauri::State<AppState>,
    directorio: String,
    max_backups: usize,
) -> Result<Option<BackupMetadataDto>, UIError> {
    let db = state
        .ledger
        .lock()
        .map_err(|_| UIError::new("db bloqueada", ""))?;
    let meta = db.auto_backup(&directorio, max_backups)?;
    Ok(meta.map(|m| BackupMetadataDto {
        version: m.version,
        timestamp_unix: m.timestamp_unix,
        arboles: m.arboles,
        total_registros: m.total_registros,
        checksum_sha256: m.checksum_sha256,
    }))
}

#[tauri::command]
fn get_backup_dir() -> Result<String, UIError> {
    let base_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
    let dir = base_dir.join("Respaldos");
    std::fs::create_dir_all(&dir)
        .map_err(|e| UIError::new("error creando directorio de respaldos", &e.to_string()))?;
    Ok(dir.to_string_lossy().to_string())
}

// ---------------- comandos: categorias ----------------

#[tauri::command]
fn listar_categorias(estado: tauri::State<AppState>) -> Result<Vec<Categoria>, UIError> {
    config_requerida(&estado)?;
    Ok(con_ledger(&estado, |db| db.listar_categorias())?)
}

#[tauri::command]
fn crear_categoria(
    estado: tauri::State<AppState>,
    nombre: String,
) -> Result<Vec<Categoria>, UIError> {
    config_requerida(&estado)?;
    let nombre = nombre.trim().to_string();
    if nombre.is_empty() || nombre.len() > 32 {
        return Err(UIError::new("nombre invalido", "1-32 caracteres"));
    }
    let cat = Categoria {
        id: format!("cat-{}", Uuid::new_v4().to_string()[..8].to_string()),
        nombre,
    };
    con_ledger(&estado, |db| db.guardar_categoria(&cat))?;
    Ok(con_ledger(&estado, |db| db.listar_categorias())?)
}

#[tauri::command]
fn eliminar_categoria(
    estado: tauri::State<AppState>,
    id: String,
) -> Result<Vec<Categoria>, UIError> {
    config_requerida(&estado)?;
    con_ledger(&estado, |db| db.eliminar_categoria(&id))?;
    Ok(con_ledger(&estado, |db| db.listar_categorias())?)
}

// ---------------- comandos: tasas impuestos ----------------

#[tauri::command]
fn listar_tasas_impuestos(estado: tauri::State<AppState>) -> Result<Vec<TasaImpuesto>, UIError> {
    config_requerida(&estado)?;
    Ok(con_ledger(&estado, |db| db.listar_tasas_impuestos())?)
}

#[tauri::command]
fn crear_tasa_impuesto(
    estado: tauri::State<AppState>,
    nombre: String,
    porcentaje: f64,
) -> Result<Vec<TasaImpuesto>, UIError> {
    config_requerida(&estado)?;
    let nombre = nombre.trim().to_string();
    if nombre.is_empty() || nombre.len() > 32 {
        return Err(UIError::new("nombre invalido", "1-32 caracteres"));
    }
    if porcentaje < 0.0 || porcentaje > 100.0 {
        return Err(UIError::new("porcentaje invalido", "0-100"));
    }
    let t = TasaImpuesto {
        id: format!("ti-{}", Uuid::new_v4().to_string()[..8].to_string()),
        nombre,
        porcentaje: Decimal::try_from(porcentaje).unwrap_or(Decimal::ZERO),
    };
    con_ledger(&estado, |db| db.guardar_tasa_impuesto(&t))?;
    Ok(con_ledger(&estado, |db| db.listar_tasas_impuestos())?)
}

#[tauri::command]
fn eliminar_tasa_impuesto(
    estado: tauri::State<AppState>,
    id: String,
) -> Result<Vec<TasaImpuesto>, UIError> {
    config_requerida(&estado)?;
    con_ledger(&estado, |db| db.eliminar_tasa_impuesto(&id))?;
    Ok(con_ledger(&estado, |db| db.listar_tasas_impuestos())?)
}

// ---------------- comandos: metodos de pago ----------------

#[tauri::command]
fn listar_metodos_pago(estado: tauri::State<AppState>) -> Result<Vec<MetodoPagoConfig>, UIError> {
    config_requerida(&estado)?;
    Ok(con_ledger(&estado, |db| db.listar_metodos_pago())?)
}

#[tauri::command]
fn crear_metodo_pago(
    estado: tauri::State<AppState>,
    nombre: String,
    moneda: String,
) -> Result<Vec<MetodoPagoConfig>, UIError> {
    config_requerida(&estado)?;
    let nombre = nombre.trim().to_uppercase().to_string();
    if nombre.is_empty() || nombre.len() > 25 {
        return Err(UIError::new(
            "nombre invalido",
            "1-25 caracteres, solo alfanumerico",
        ));
    }
    if moneda != "USD" && moneda != "BS" {
        return Err(UIError::new("moneda invalida", "USD o BS"));
    }
    // Nombres unicos: dos metodos con el mismo nombre en distinta moneda
    // vuelven ambiguo el selector y pueden mostrar un metodo y grabar otro.
    if con_ledger(&estado, |db| db.listar_metodos_pago())?
        .iter()
        .any(|m| m.nombre == nombre)
    {
        return Err(UIError::new(
            "metodo duplicado",
            "Ya existe un metodo de pago con ese nombre",
        ));
    }
    let m = MetodoPagoConfig { nombre, moneda };
    con_ledger(&estado, |db| db.guardar_metodo_pago(&m))?;
    Ok(con_ledger(&estado, |db| db.listar_metodos_pago())?)
}

#[tauri::command]
fn eliminar_metodo_pago(
    estado: tauri::State<AppState>,
    nombre: String,
) -> Result<Vec<MetodoPagoConfig>, UIError> {
    config_requerida(&estado)?;
    let metodos = con_ledger(&estado, |db| db.listar_metodos_pago())?;
    if metodos.len() <= 1 {
        return Err(UIError::new(
            "no permitido",
            "Debe existir al menos un metodo de pago",
        ));
    }
    con_ledger(&estado, |db| db.eliminar_metodo_pago(&nombre))?;
    Ok(con_ledger(&estado, |db| db.listar_metodos_pago())?)
}

// ---------------- comandos: operadores ----------------

#[tauri::command]
fn listar_operadores(estado: tauri::State<AppState>) -> Result<Vec<Operador>, UIError> {
    config_requerida(&estado)?;
    Ok(con_ledger(&estado, |db| db.listar_operadores())?)
}

#[tauri::command]
fn crear_operador(
    estado: tauri::State<AppState>,
    nombre: String,
) -> Result<Vec<Operador>, UIError> {
    config_requerida(&estado)?;
    let nombre = nombre.trim().to_string();
    if nombre.is_empty() || nombre.len() > 30 {
        return Err(UIError::new("nombre invalido", "1-30 caracteres"));
    }
    let op = Operador {
        id: format!("op-{}", Uuid::new_v4().to_string()[..8].to_string()),
        nombre,
        activo: true,
        creado_unix: ahora_unix(),
    };
    con_ledger(&estado, |db| db.guardar_operador(&op))?;
    Ok(con_ledger(&estado, |db| db.listar_operadores())?)
}

#[tauri::command]
fn editar_operador(
    estado: tauri::State<AppState>,
    id: String,
    nombre: String,
) -> Result<Vec<Operador>, UIError> {
    config_requerida(&estado)?;
    let nombre = nombre.trim().to_string();
    if nombre.is_empty() || nombre.len() > 30 {
        return Err(UIError::new("nombre invalido", "1-30 caracteres"));
    }
    let mut ops = con_ledger(&estado, |db| db.listar_operadores())?;
    if let Some(op) = ops.iter_mut().find(|o| o.id == id) {
        op.nombre = nombre;
        con_ledger(&estado, |db| db.guardar_operador(op))?;
    }
    Ok(con_ledger(&estado, |db| db.listar_operadores())?)
}

#[tauri::command]
fn eliminar_operador(estado: tauri::State<AppState>, id: String) -> Result<Vec<Operador>, UIError> {
    config_requerida(&estado)?;
    con_ledger(&estado, |db| db.eliminar_operador(&id))?;
    Ok(con_ledger(&estado, |db| db.listar_operadores())?)
}

#[tauri::command]
fn alternar_operador(estado: tauri::State<AppState>, id: String) -> Result<Vec<Operador>, UIError> {
    config_requerida(&estado)?;
    let mut ops = con_ledger(&estado, |db| db.listar_operadores())?;
    if let Some(op) = ops.iter_mut().find(|o| o.id == id) {
        op.activo = !op.activo;
        con_ledger(&estado, |db| db.guardar_operador(op))?;
    }
    Ok(con_ledger(&estado, |db| db.listar_operadores())?)
}

// ---------------- comandos: semaforo stock ----------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SemaforoDto {
    rojo_max: u32,
    amarillo_max: u32,
}

#[tauri::command]
fn obtener_semaforo_stock(estado: tauri::State<AppState>) -> Result<SemaforoDto, UIError> {
    config_requerida(&estado)?;
    let s = con_ledger(&estado, |db| db.cargar_semaforo())?;
    Ok(s.map(|v| SemaforoDto {
        rojo_max: v.rojo_max,
        amarillo_max: v.amarillo_max,
    })
    .unwrap_or(SemaforoDto {
        rojo_max: 5,
        amarillo_max: 15,
    }))
}

#[tauri::command]
fn guardar_semaforo_stock(
    estado: tauri::State<AppState>,
    rojo_max: u32,
    amarillo_max: u32,
) -> Result<SemaforoDto, UIError> {
    config_requerida(&estado)?;
    if rojo_max < 1 || amarillo_max <= rojo_max {
        return Err(UIError::new(
            "valores invalidos",
            "Rojo >= 1, amarillo > rojo",
        ));
    }
    let s = SemaforoStock {
        rojo_max,
        amarillo_max,
    };
    con_ledger(&estado, |db| db.guardar_semaforo(&s))?;
    Ok(SemaforoDto {
        rojo_max,
        amarillo_max,
    })
}

// ---------------- comandos: cambiar PIN ----------------

#[tauri::command]
fn cambiar_pin_dueno(
    estado: tauri::State<AppState>,
    pin_anterior: String,
    pin_nuevo: String,
) -> Result<bool, UIError> {
    config_requerida(&estado)?;
    let mut cfg = con_ledger(&estado, |db| db.cargar_config())?
        .ok_or_else(|| UIError::new("negocio sin inicializar", "Ejecute el asistente"))?;
    if !cfg.pin_dueno_sha256.is_empty() {
        if cfg.pin_dueno_sha256 != hash_pin(&pin_anterior) {
            return Err(UIError::new(
                "pin incorrecto",
                "La clave anterior no coincide",
            ));
        }
    }
    // Validate new PIN length
    if !pin_nuevo.trim().is_empty() && pin_nuevo.trim().len() < 6 {
        return Err(UIError::new(
            "pin muy corto",
            "El PIN debe tener al menos 6 dígitos",
        ));
    }
    cfg.pin_dueno_sha256 = if pin_nuevo.trim().is_empty() {
        String::new()
    } else {
        hash_pin(&pin_nuevo)
    };
    con_ledger(&estado, |db| db.actualizar_config(&cfg))?;
    Ok(true)
}

// ---------------- comandos: actualizar producto ----------------

#[tauri::command]
fn actualizar_producto(
    estado: tauri::State<AppState>,
    input: ProductoInput,
) -> Result<(), UIError> {
    let cfg = config_requerida(&estado)?;
    if input.perecedero && capacidades_de_rubros(cfg.rubros) & capacidades::CAP_PERECEDERO == 0 {
        return Err(UIError::from(ErrorNegocio::CapacidadInactiva(
            capacidades::CAP_PERECEDERO,
        )));
    }
    let sku_norm = input.sku.trim().to_uppercase();
    let sku_obj = Sku::new(&sku_norm).map_err(|e| UIError::new("SKU invalido", e))?;
    let db = estado
        .ledger
        .lock()
        .map_err(|_| UIError::new("db bloqueada", ""))?;
    let tree = db
        .inner_db()
        .open_tree("productos")
        .map_err(|e| UIError::new("error db", &e.to_string()))?;

    let raw = tree
        .get(sku_obj.as_bytes())
        .map_err(|e| UIError::new("error db", &e.to_string()))?
        .ok_or_else(|| UIError::new("producto inexistente", "El SKU indicado no existe"))?;

    let mut producto: Producto = bincode::deserialize(&raw)
        .or_else(|_| serde_json::from_slice(&raw))
        .map_err(|e| UIError::new("error deserializacion", &e.to_string()))?;

    if !input.nombre.trim().is_empty() {
        producto.nombre =
            Nombre::new(input.nombre.trim()).map_err(|e| UIError::new("nombre invalido", e))?;
    }
    if let Ok(p) = decimal_de(&input.precio_usd) {
        if p > Decimal::ZERO {
            producto.precio_usd = p;
        }
    }
    if let Ok(imp) = decimal_de(&input.impuesto_pct) {
        producto.impuesto_pct = imp;
    }
    if let Some(ref pb) = input.precio_bruto_usd {
        producto.precio_bruto_usd = decimal_de(pb).ok();
    }
    if let Some(ref mg) = input.margen_pct {
        producto.margen_pct = decimal_de(mg).ok();
    }
    if let Some(cat) = input.categoria_id {
        producto.categoria_id = if cat.trim().is_empty() {
            None
        } else {
            Some(cat.trim().to_string())
        };
    }
    if let Some(ss) = input.sin_stock {
        producto.sin_stock = ss;
    }
    if let Some(ref un) = input.unidad {
        producto.unidad = Some(un.clone());
    }
    if let Some(ec) = input.es_caja {
        producto.es_caja = ec;
    }
    if input.unidades_por_caja.is_some() {
        producto.unidades_por_caja = input.unidades_por_caja;
    }
    if let Some(ref pp) = input.precio_paquete_usd {
        producto.precio_paquete_usd = decimal_de(pp).ok();
    }
    if let Some(ref np) = input.nombre_paquete {
        producto.nombre_paquete = Some(np.clone());
    }
    if let Some(pres) = input.presentaciones {
        producto.presentaciones = pres
            .into_iter()
            .filter_map(|p| {
                decimal_de(&p.precio_usd).ok().map(|precio_usd| {
                    datiolabs_core::models::Presentacion {
                        nombre: p.nombre,
                        precio_usd,
                        unidades: p.unidades.max(1),
                    }
                })
            })
            .collect();
    }
    // El formulario controla la capacidad de perecedero: se pone o se quita.
    if input.perecedero {
        producto.capacidades |= capacidades::CAP_PERECEDERO;
    } else {
        producto.capacidades &= !capacidades::CAP_PERECEDERO;
    }

    db.actualizar_producto(&producto)
        .map_err(|e| UIError::new("error actualizando producto", &e.to_string()))?;
    drop(db);
    Ok(())
}

// ---------------- comandos: eliminar producto ----------------

#[tauri::command]
fn eliminar_producto(estado: tauri::State<AppState>, sku: String) -> Result<(), UIError> {
    config_requerida(&estado)?;
    let sku_norm = sku.trim().to_uppercase();
    let db = estado
        .ledger
        .lock()
        .map_err(|_| UIError::new("db bloqueada", ""))?;
    let tree = db
        .inner_db()
        .open_tree("productos")
        .map_err(|e| UIError::new("error db", &e.to_string()))?;
    tree.remove(sku_norm.as_bytes())
        .map_err(|e| UIError::new("error eliminando", &e.to_string()))?;
    drop(db);
    Ok(())
}

// ---------------- comandos: reducir stock ----------------

#[tauri::command]
fn reducir_stock(
    estado: tauri::State<AppState>,
    sku: String,
    cantidad: String,
) -> Result<Decimal, UIError> {
    config_requerida(&estado)?;
    let cant = decimal_de(&cantidad)?;
    if cant <= Decimal::ZERO {
        return Err(UIError::new("cantidad invalida", "Debe ser mayor a cero"));
    }
    let mov = movimiento(
        sku.trim().to_uppercase().as_str(),
        -cant,
        MotivoMovimiento::Ajuste,
        None,
    );
    let nuevo_stock = con_ledger(&estado, |db| -> Result<Decimal, DbError> {
        db.aplicar_movimiento(mov, |_, _| {})?;
        let cat = db.cargar_catalogo()?;
        let idx = cat
            .indice_de(sku.trim().to_uppercase().as_str())
            .ok_or(DbError::Negocio(ErrorNegocio::ProductoInexistente))?;
        Ok(cat.stock(idx))
    })?;
    Ok(nuevo_stock)
}

// ---------------- comandos: jornada ----------------

#[tauri::command]
fn obtener_jornada_actual(estado: tauri::State<AppState>) -> Result<Option<Jornada>, UIError> {
    config_requerida(&estado)?;
    Ok(con_ledger(&estado, |db| db.jornada_actual())?)
}

/// Latido de la interfaz: el frontend lo envia cada pocos segundos. Si deja de
/// llegar, el vigilante de `run()` recarga la ventana (renderer colgado/caido).
#[tauri::command]
fn latido(estado: tauri::State<AppState>) -> Result<bool, UIError> {
    estado
        .ultimo_latido
        .store(ahora_unix(), std::sync::atomic::Ordering::Relaxed);
    Ok(true)
}

#[tauri::command]
fn listar_historico_jornadas(estado: tauri::State<AppState>) -> Result<Vec<Jornada>, UIError> {
    config_requerida(&estado)?;
    Ok(con_ledger(&estado, |db| db.listar_jornadas(50))?)
}

#[tauri::command]
fn abrir_jornada(
    estado: tauri::State<AppState>,
    operador: String,
    operadores: Option<Vec<String>>,
) -> Result<Jornada, UIError> {
    config_requerida(&estado)?;
    // Se puede abrir turno aunque aun no haya tasa: la tasa se exige al
    // cobrar, no al abrir. La jornada registra la tasa vigente (puede ser 0).
    let tasa = estado.servicio_tasa.info_actual().valor;
    let actual = con_ledger(&estado, |db| db.jornada_actual())?;
    if actual.is_some() {
        return Err(UIError::new(
            "jornada abierta",
            "Ya existe una jornada laboral abierta",
        ));
    }
    let ops = operadores.unwrap_or_default();
    let jornada = Jornada {
        id: format!(
            "JOR-{}-{}",
            chrono::Utc::now().format("%Y%m%d"),
            Uuid::new_v4().to_string()[..6].to_uppercase()
        ),
        estado: "abierta".to_string(),
        inicio_unix: ahora_unix(),
        fin_unix: None,
        operador_inicial: operador.clone(),
        operador_actual: operador,
        operadores_activos: ops.clone(),
        operadores_relevo: ops,
        tasa_inicio: tasa,
        tasa_fin: None,
        ventas_total_usd: Decimal::ZERO,
        ventas_total_bs: Decimal::ZERO,
        tickets_emitidos: 0,
        vuelto_pagado_bs: Decimal::ZERO,
        vuelto_retenido_bs: Decimal::ZERO,
        deudas_liquidadas_usd: Decimal::ZERO,
        entradas_stock_reg: 0,
        mermas_stock_reg: 0,
        cambios_precio_reg: 0,
        checksum_sha256: None,
    };
    con_ledger(&estado, |db| db.guardar_jornada(&jornada))?;
    Ok(jornada)
}

#[tauri::command]
fn asignar_operadores_turno(
    estado: tauri::State<AppState>,
    operadores: Vec<String>,
) -> Result<Jornada, UIError> {
    config_requerida(&estado)?;
    let mut jornada = con_ledger(&estado, |db| db.jornada_actual())?
        .ok_or_else(|| UIError::new("sin jornada", "No hay jornada abierta"))?;
    jornada.operadores_activos = operadores.clone();
    for op in &operadores {
        if !jornada.operadores_relevo.contains(op) {
            jornada.operadores_relevo.push(op.clone());
        }
    }
    con_ledger(&estado, |db| db.guardar_jornada(&jornada))?;
    Ok(jornada)
}

#[tauri::command]
fn relevar_operador_jornada(
    estado: tauri::State<AppState>,
    operador: String,
) -> Result<Jornada, UIError> {
    config_requerida(&estado)?;
    let mut jornada = con_ledger(&estado, |db| db.jornada_actual())?
        .ok_or_else(|| UIError::new("sin jornada", "No hay jornada abierta"))?;
    jornada.operador_actual = operador.clone();
    jornada.operadores_activos = vec![operador.clone()];
    if !jornada.operadores_relevo.contains(&operador) {
        jornada.operadores_relevo.push(operador);
    }
    con_ledger(&estado, |db| db.guardar_jornada(&jornada))?;
    Ok(jornada)
}

#[tauri::command]
fn cerrar_jornada(estado: tauri::State<AppState>) -> Result<Jornada, UIError> {
    config_requerida(&estado)?;
    let tasa = tasa_viva(&estado)?;
    // Regla de rigor: no se cierra un turno con cuentas abiertas; primero se
    // pasan a deuda para que el arqueo quede cuadrado.
    let cuentas = con_ledger(&estado, |db| db.ventas_recientes(2_000))?;
    // Las deudas ya no bloquean: son cartera a cobrar, no consumo en curso.
    let abiertas: Vec<String> = cuentas
        .into_iter()
        .filter(|v| v.es_cuenta_abierta && v.estado == EstadoVenta::Abierta && v.tipo != "deuda")
        .map(|v| v.etiqueta)
        .collect();
    if !abiertas.is_empty() {
        return Err(UIError::new(
            format!("{} cuentas activas abiertas", abiertas.len()),
            "Pase las cuentas a deuda (o liquide) antes de cerrar la jornada",
        ));
    }
    let mut jornada = con_ledger(&estado, |db| db.jornada_actual())?
        .ok_or_else(|| UIError::new("sin jornada", "No hay jornada abierta para cerrar"))?;
    jornada.estado = "cerrada".to_string();
    jornada.fin_unix = Some(ahora_unix());
    jornada.tasa_fin = Some(tasa);
    let checksum = format!("{:x}", Sha256::digest(jornada.id.as_bytes()));
    jornada.checksum_sha256 = Some(checksum);
    con_ledger(&estado, |db| db.guardar_jornada(&jornada))?;
    Ok(jornada)
}

// ---------------- comandos: cuentas (eliminar consumo, abonar, editar abono) ----------------

#[tauri::command]
fn eliminar_consumo(
    estado: tauri::State<AppState>,
    venta_id: String,
    consumo_id: String,
    cantidad: Option<String>,
) -> Result<CuentaDto, UIError> {
    config_requerida(&estado)?;
    jornada_requerida(&estado)?;
    // Parse index from consumption ID (format: "consumo-{index}")
    let consumo_idx = consumo_id
        .strip_prefix("consumo-")
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or_else(|| UIError::new("consumo invalido", "El ID del consumo no es valido"))?;
    // Cantidad parcial a descontar: permite "quitar una sola unidad" sin borrar
    // la linea. Sin cantidad (o con una igual/mayor a la de la linea) se elimina
    // la linea completa, que es lo que hace el boton de borrar.
    let parcial = match cantidad.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(texto) => Some(decimal_de(texto)?),
        None => None,
    };
    let actualizada = con_ledger(&estado, |db| {
        let mut venta = db
            .cargar_venta(&venta_id)?
            .filter(|v| v.es_cuenta_abierta && v.estado == EstadoVenta::Abierta)
            .ok_or(DbError::Negocio(ErrorNegocio::CuentaInvalida(
                venta_id.clone(),
            )))?;
        if consumo_idx >= venta.lineas.skus.len() {
            return Err(DbError::Negocio(ErrorNegocio::CuentaInvalida(venta_id)));
        }
        let sku = venta.lineas.skus[consumo_idx];
        let cantidad = venta.lineas.cantidades[consumo_idx];
        let modo_linea = venta
            .lineas
            .modos_venta
            .get(consumo_idx)
            .cloned()
            .unwrap_or_else(|| "unidad".to_string());
        let restante = match parcial {
            Some(c) if c > Decimal::ZERO && c < cantidad => cantidad - c,
            _ => Decimal::ZERO,
        };
        let elimina_linea = restante <= Decimal::ZERO;
        // Unidades que salen de la linea (y por tanto vuelven al inventario).
        let cantidad_a_quitar = cantidad - restante;
        if elimina_linea {
            if consumo_idx < venta.lineas.modos_venta.len() {
                venta.lineas.modos_venta.remove(consumo_idx);
            }
            venta.lineas.skus.remove(consumo_idx);
            venta.lineas.nombres.remove(consumo_idx);
            venta.lineas.cantidades.remove(consumo_idx);
            venta.lineas.precios_usd.remove(consumo_idx);
            venta.lineas.tasas_bloqueadas.remove(consumo_idx);
        } else {
            venta.lineas.cantidades[consumo_idx] = restante;
        }
        let tree_p = db.inner_db().open_tree("productos")?;
        let prod_opt = tree_p.get(sku.as_bytes())?.and_then(|v| {
            bincode::deserialize::<Producto>(&v)
                .or_else(|_| serde_json::from_slice::<Producto>(&v))
                .ok()
        });
        let sin_stock = prod_opt.as_ref().map(|p| p.sin_stock).unwrap_or(false);
        if !sin_stock {
            let mut factor: u32 = 1;
            if let Some(ref p) = prod_opt {
                if let Some(pres) = p
                    .presentaciones
                    .iter()
                    .find(|x| x.nombre.eq_ignore_ascii_case(&modo_linea))
                {
                    factor = pres.unidades.max(1);
                } else if modo_linea == "paquete" && p.es_caja {
                    factor = p.unidades_por_caja.unwrap_or(1).max(1);
                }
            }
            let unidades_a_reintegrar = cantidad_a_quitar * Decimal::from(factor);
            let mov = MovimientoStock {
                id: Uuid::new_v4().to_string(),
                sku: sku.as_str().to_string(),
                delta: unidades_a_reintegrar,
                motivo: MotivoMovimiento::Ajuste,
                venta_id: Some(venta_id),
                fecha_unix: ahora_unix(),
                firma_sha256: String::new(),
            };
            db.aplicar_movimiento(mov, |_, _| {})?;
            let caps = prod_opt.as_ref().map(|p| p.capacidades).unwrap_or(0);
            reintegrar_lotes_si_perecedero(db, sku.as_str(), caps, unidades_a_reintegrar)?;
        }
        db.guardar_venta(venta)
    })?;
    Ok(cuenta_dto(&actualizada))
}

#[tauri::command]
fn abonar_cuenta(
    estado: tauri::State<AppState>,
    venta_id: String,
    monto_usd: Option<f64>,
    monto_bs: Option<f64>,
) -> Result<CuentaDto, UIError> {
    config_requerida(&estado)?;
    jornada_requerida(&estado)?;
    // Rigor: un abono invalido no puede convertirse en cero en silencio.
    let usd = match monto_usd {
        Some(v) if v.is_finite() => Decimal::try_from(v).map_err(|_| {
            UIError::new("monto invalido", "El monto en USD no es un decimal valido")
        })?,
        Some(_) => {
            return Err(UIError::new(
                "monto invalido",
                "El monto en USD no es un numero finito",
            ));
        }
        None => Decimal::ZERO,
    };
    let bs = match monto_bs {
        Some(v) if v.is_finite() => Decimal::try_from(v).map_err(|_| {
            UIError::new("monto invalido", "El monto en Bs no es un decimal valido")
        })?,
        Some(_) => {
            return Err(UIError::new(
                "monto invalido",
                "El monto en Bs no es un numero finito",
            ));
        }
        None => Decimal::ZERO,
    };
    if usd < Decimal::ZERO || bs < Decimal::ZERO {
        return Err(UIError::new(
            "monto invalido",
            "El abono no puede ser negativo",
        ));
    }
    if usd == Decimal::ZERO && bs == Decimal::ZERO {
        return Err(UIError::new(
            "monto invalido",
            "El abono debe ser mayor a cero",
        ));
    }
    let actualizada = con_ledger(&estado, |db| {
        let mut venta = db
            .cargar_venta(&venta_id)?
            .filter(|v| v.es_cuenta_abierta && v.estado == EstadoVenta::Abierta)
            .ok_or(DbError::Negocio(ErrorNegocio::CuentaInvalida(venta_id)))?;

        // Reject payments on accounts with no products — prevents floating data
        if venta.lineas.skus.is_empty() && venta.total_usd <= Decimal::ZERO {
            return Err(DbError::Negocio(ErrorNegocio::CuentaInvalida(
                "No se puede abonar a una cuenta sin productos registrados".into(),
            )));
        }

        venta.abonos_usd = Some(venta.abonos_usd.unwrap_or(Decimal::ZERO) + usd);
        venta.abonos_bs = Some(venta.abonos_bs.unwrap_or(Decimal::ZERO) + bs);
        db.guardar_venta(venta)
    })?;
    Ok(cuenta_dto(&actualizada))
}

#[tauri::command]
fn editar_abono_cuenta(
    estado: tauri::State<AppState>,
    venta_id: String,
    nuevo_abono_usd: f64,
) -> Result<CuentaDto, UIError> {
    config_requerida(&estado)?;
    jornada_requerida(&estado)?;
    if !nuevo_abono_usd.is_finite() {
        return Err(UIError::new(
            "monto invalido",
            "El nuevo abono no es un numero finito",
        ));
    }
    let usd = Decimal::try_from(nuevo_abono_usd)
        .map_err(|_| UIError::new("monto invalido", "El nuevo abono no es un decimal valido"))?;
    if usd < Decimal::ZERO {
        return Err(UIError::new(
            "monto invalido",
            "El abono no puede ser negativo",
        ));
    }
    let actualizada = con_ledger(&estado, |db| {
        let mut venta = db
            .cargar_venta(&venta_id)?
            .filter(|v| v.es_cuenta_abierta && v.estado == EstadoVenta::Abierta)
            .ok_or(DbError::Negocio(ErrorNegocio::CuentaInvalida(venta_id)))?;
        venta.abonos_usd = Some(usd);
        db.guardar_venta(venta)
    })?;
    Ok(cuenta_dto(&actualizada))
}

#[tauri::command]
fn convertir_cuenta_a_deuda(
    estado: tauri::State<AppState>,
    venta_id: String,
    cliente: String,
    nota: Option<String>,
) -> Result<CuentaDto, UIError> {
    config_requerida(&estado)?;
    let actualizada = con_ledger(&estado, |db| {
        let mut venta = db
            .cargar_venta(&venta_id)?
            .filter(|v| v.es_cuenta_abierta && v.estado == EstadoVenta::Abierta)
            .ok_or(DbError::Negocio(ErrorNegocio::CuentaInvalida(venta_id)))?;
        venta.tipo = "deuda".to_string();
        let c = cliente.trim().to_string();
        if !c.is_empty() {
            venta.etiqueta = c.clone();
            venta.cliente = Some(c);
        }
        if let Some(n) = nota {
            venta.nota = Some(n.trim().to_string());
        }
        db.guardar_venta(venta)
    })?;
    Ok(cuenta_dto(&actualizada))
}

#[tauri::command]
fn editar_cuenta(
    estado: tauri::State<AppState>,
    venta_id: String,
    etiqueta: Option<String>,
    cliente: Option<String>,
    nota: Option<String>,
) -> Result<CuentaDto, UIError> {
    config_requerida(&estado)?;
    let actualizada = con_ledger(&estado, |db| {
        let mut venta = db
            .cargar_venta(&venta_id)?
            .filter(|v| v.es_cuenta_abierta && v.estado == EstadoVenta::Abierta)
            .ok_or(DbError::Negocio(ErrorNegocio::CuentaInvalida(venta_id)))?;
        if let Some(etq) = etiqueta {
            let t = etq.trim().to_string();
            if !t.is_empty() {
                venta.etiqueta = t;
            }
        }
        if let Some(c) = cliente {
            let t = c.trim().to_string();
            if !t.is_empty() {
                venta.cliente = Some(t);
            } else {
                venta.cliente = None;
            }
        }
        if let Some(n) = nota {
            let t = n.trim().to_string();
            if !t.is_empty() {
                venta.nota = Some(t);
            } else {
                venta.nota = None;
            }
        }
        db.guardar_venta(venta)
    })?;
    Ok(cuenta_dto(&actualizada))
}

// ---------------- comandos: listar y eliminar ventas ----------------

#[tauri::command]
fn eliminar_venta(estado: tauri::State<AppState>, venta_id: String) -> Result<bool, UIError> {
    config_requerida(&estado)?;
    jornada_requerida(&estado)?;
    con_ledger(&estado, |db| -> Result<bool, DbError> {
        let jornada = db
            .jornada_actual()?
            .ok_or(DbError::Negocio(ErrorNegocio::NoInicializado))?;
        let venta = db.cargar_venta(&venta_id)?.ok_or_else(|| {
            DbError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Venta no encontrada",
            ))
        })?;
        // Rigor: solo se anulan ventas del turno abierto; los turnos cerrados
        // son inmutables. Toda anulacion queda auditada con su operador.
        if venta.fecha_cierre_unix < jornada.inicio_unix {
            return Err(DbError::Negocio(ErrorNegocio::FueraDeTurno));
        }
        db.eliminar_venta(&venta_id)?;

        let operador = if !jornada.operadores_activos.is_empty() {
            Some(jornada.operadores_activos.join(", "))
        } else if !jornada.operador_actual.is_empty() {
            Some(jornada.operador_actual.clone())
        } else {
            None
        };
        db.guardar_anulacion(&Anulacion {
            id: Uuid::new_v4().to_string(),
            venta: venta.clone(),
            fecha_anulacion_unix: ahora_unix(),
            jornada_id: Some(jornada.id.clone()),
            jornada_origen_id: Some(jornada.id.clone()),
            operador: operador.clone(),
            motivo: Some("ERROR OPERATIVO".to_string()),
            firma_sha256: String::new(),
        })?;

        // Si la venta anulada tenia redondeos retenidos, se reversan con un
        // asiento negativo para que el arqueo vuelva a cuadrar (auditable).
        for r in db
            .listar_redondeos(2_000)?
            .into_iter()
            .filter(|r| r.venta_id == venta.id && r.monto_bs > Decimal::ZERO)
        {
            db.guardar_redondeo(&Redondeo {
                id: Uuid::new_v4().to_string(),
                venta_id: r.venta_id.clone(),
                monto_bs: -r.monto_bs,
                monto_usd: r.monto_usd.map(|m| -m),
                tasa: r.tasa,
                fecha_unix: ahora_unix(),
                jornada_id: Some(jornada.id.clone()),
                operador: operador.clone(),
                metodo: Some("ANULACION".to_string()),
                firma_sha256: String::new(),
            })?;
        }

        // Reintegrar stock de cada línea si el producto no es sin_stock
        let tree_p = db.inner_db().open_tree("productos")?;
        for i in 0..venta.lineas.skus.len() {
            let sku_obj = venta.lineas.skus[i];
            let cant = venta.lineas.cantidades[i];
            let modo_linea = venta
                .lineas
                .modos_venta
                .get(i)
                .map(|s| s.as_str())
                .unwrap_or("unidad");

            let prod_opt = tree_p.get(sku_obj.as_bytes())?.and_then(|v| {
                bincode::deserialize::<Producto>(&v)
                    .or_else(|_| serde_json::from_slice::<Producto>(&v))
                    .ok()
            });

            let sin_stock = prod_opt.as_ref().map(|p| p.sin_stock).unwrap_or(false);
            if !sin_stock {
                let mut factor: u32 = 1;
                if let Some(ref p) = prod_opt {
                    if let Some(pres) = p
                        .presentaciones
                        .iter()
                        .find(|x| x.nombre.eq_ignore_ascii_case(modo_linea))
                    {
                        factor = pres.unidades.max(1);
                    } else if modo_linea == "paquete" && p.es_caja {
                        factor = p.unidades_por_caja.unwrap_or(1).max(1);
                    }
                }
                let unidades_a_reintegrar = cant * Decimal::from(factor);
                let mov = MovimientoStock {
                    id: Uuid::new_v4().to_string(),
                    sku: sku_obj.as_str().to_string(),
                    delta: unidades_a_reintegrar,
                    motivo: MotivoMovimiento::Ajuste,
                    venta_id: Some(venta_id.clone()),
                    fecha_unix: ahora_unix(),
                    firma_sha256: String::new(),
                };
                db.aplicar_movimiento(mov, |_, _| {})?;
                let caps = prod_opt.as_ref().map(|p| p.capacidades).unwrap_or(0);
                reintegrar_lotes_si_perecedero(db, sku_obj.as_str(), caps, unidades_a_reintegrar)?;
            }
        }

        // Ajustar contadores de la jornada activa si pertenecía a la misma
        if let Ok(Some(mut jornada)) = db.jornada_actual() {
            if venta.fecha_cierre_unix >= jornada.inicio_unix {
                if jornada.ventas_total_usd >= venta.total_usd {
                    jornada.ventas_total_usd -= venta.total_usd;
                } else {
                    jornada.ventas_total_usd = Decimal::ZERO;
                }
                if jornada.ventas_total_bs >= venta.total_bs {
                    jornada.ventas_total_bs -= venta.total_bs;
                } else {
                    jornada.ventas_total_bs = Decimal::ZERO;
                }
                if jornada.tickets_emitidos > 0 {
                    jornada.tickets_emitidos -= 1;
                }
                if venta.tipo == "deuda" {
                    if jornada.deudas_liquidadas_usd >= venta.total_usd {
                        jornada.deudas_liquidadas_usd -= venta.total_usd;
                    } else {
                        jornada.deudas_liquidadas_usd = Decimal::ZERO;
                    }
                }
                if let Some("PAGADO") = venta.estado_vuelto.as_deref() {
                    if jornada.vuelto_pagado_bs >= venta.vuelto_bs {
                        jornada.vuelto_pagado_bs -= venta.vuelto_bs;
                    } else {
                        jornada.vuelto_pagado_bs = Decimal::ZERO;
                    }
                } else if let Some("RETENIDO") = venta.estado_vuelto.as_deref() {
                    if jornada.vuelto_retenido_bs >= venta.vuelto_bs {
                        jornada.vuelto_retenido_bs -= venta.vuelto_bs;
                    } else {
                        jornada.vuelto_retenido_bs = Decimal::ZERO;
                    }
                }
                db.guardar_jornada(&jornada)?;
            }
        }

        Ok(true)
    })?;
    Ok(true)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AnulacionDto {
    id: String,
    venta_id: String,
    etiqueta: String,
    total_usd: Decimal,
    total_bs: Decimal,
    fecha_anulacion_unix: i64,
    jornada_id: Option<String>,
    operador: Option<String>,
    motivo: Option<String>,
    firma_sha256: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RedondeoDto {
    id: String,
    venta_id: String,
    monto_bs: Decimal,
    monto_usd: Option<Decimal>,
    tasa: Decimal,
    fecha_unix: i64,
    jornada_id: Option<String>,
    operador: Option<String>,
    metodo: Option<String>,
    firma_sha256: String,
}

/// Redondeos voluntarios retenidos: ingreso auditable por venta y turno.
#[tauri::command]
fn listar_redondeos(
    estado: tauri::State<AppState>,
    limite: Option<usize>,
) -> Result<Vec<RedondeoDto>, UIError> {
    config_requerida(&estado)?;
    let redondeos = con_ledger(&estado, |db| db.listar_redondeos(limite.unwrap_or(500)))?;
    Ok(redondeos
        .into_iter()
        .map(|r| RedondeoDto {
            id: r.id,
            venta_id: r.venta_id,
            monto_bs: r.monto_bs,
            monto_usd: r.monto_usd,
            tasa: r.tasa,
            fecha_unix: r.fecha_unix,
            jornada_id: r.jornada_id,
            operador: r.operador,
            metodo: r.metodo,
            firma_sha256: r.firma_sha256,
        })
        .collect())
}

/// Libro de anulaciones para auditoria: que se anulo y con que turno.
#[tauri::command]
fn listar_anulaciones(
    estado: tauri::State<AppState>,
    limite: Option<usize>,
) -> Result<Vec<AnulacionDto>, UIError> {
    config_requerida(&estado)?;
    let anulaciones = con_ledger(&estado, |db| db.listar_anulaciones(limite.unwrap_or(200)))?;
    Ok(anulaciones
        .into_iter()
        .map(|a| AnulacionDto {
            id: a.id,
            venta_id: a.venta.id,
            etiqueta: a.venta.etiqueta,
            total_usd: a.venta.total_usd,
            total_bs: a.venta.total_bs,
            fecha_anulacion_unix: a.fecha_anulacion_unix,
            jornada_id: a.jornada_id,
            operador: a.operador,
            motivo: a.motivo,
            firma_sha256: a.firma_sha256,
        })
        .collect())
}

#[tauri::command]
fn listar_ventas(estado: tauri::State<AppState>) -> Result<Vec<TicketDto>, UIError> {
    config_requerida(&estado)?;
    let ventas = con_ledger(&estado, |db| db.ventas_recientes(2_000))?;
    Ok(ventas
        .iter()
        // Solo las cuentas/deudas ABIERTAS se excluyen del historial de tickets:
        // una vez liquidadas deben verse con su detalle y su boton VER, igual
        // que en la version anterior del cliente.
        .filter(|v| !(v.es_cuenta_abierta && v.estado == EstadoVenta::Abierta))
        .map(|v| armar_ticket(v, v.monto_recibido_bs, v.vuelto_bs))
        .collect())
}

// ---------------- comandos: buscador de deudas ----------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeudaItemDto {
    id: String,
    nombre: String,
    abierta: bool,
    apertura_unix: i64,
    cierre_unix: Option<i64>,
    total_usd: String,
    abonos_usd: String,
    saldo_usd: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeudorGrupoDto {
    nombre: String,
    abiertas: u32,
    saldo_usd: String,
    historico_usd: String,
    total_deudas: u32,
    ultima_unix: i64,
    deudas: Vec<DeudaItemDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BusquedaDeudasDto {
    total_grupos: u32,
    total_deudas: u32,
    pagina: u32,
    por_pagina: u32,
    grupos: Vec<DeudorGrupoDto>,
}

fn armar_indice_deudas(estado: &AppState) -> Result<Arc<Vec<DeudaIndice>>, UIError> {
    use std::sync::atomic::Ordering;
    let version = datiolabs_core::db::VERSION_CUENTAS.load(Ordering::Relaxed);
    if let Ok(cache) = estado.cache_deudas.lock() {
        if let Some((v, lista)) = cache.as_ref() {
            if *v == version {
                // Se comparte el mismo indice: cero copias por busqueda.
                return Ok(Arc::clone(lista));
            }
        }
    }
    let ventas = con_ledger(estado, |db| db.ventas_recientes(usize::MAX))?;
    let lista = Arc::new(
        ventas
            .iter()
            .filter(|v| v.tipo == "deuda")
            .map(|v| {
                let abierta = v.es_cuenta_abierta && v.estado == EstadoVenta::Abierta;
                // En cuentas abiertas el total vive en las lineas (`total_usd` solo
                // se fija al cerrar), igual que lo hace CuentaDto.
                let total = if abierta {
                    v.lineas.total_usd()
                } else {
                    v.total_usd
                };
                let abonos = v.abonos_usd.unwrap_or(Decimal::ZERO);
                let saldo = if abierta {
                    let pendiente = total - abonos;
                    if pendiente > Decimal::ZERO {
                        pendiente
                    } else {
                        Decimal::ZERO
                    }
                } else {
                    Decimal::ZERO
                };
                DeudaIndice {
                    id: v.id.clone(),
                    nombre: v.etiqueta.clone(),
                    normalizado: normalizar_busqueda(&v.etiqueta),
                    cliente_normalizado: normalizar_busqueda(v.cliente.as_deref().unwrap_or("")),
                    abierta,
                    apertura_unix: v.fecha_apertura_unix,
                    cierre_unix: v.fecha_cierre_unix,
                    total_usd: total,
                    abonos_usd: abonos,
                    saldo_usd: saldo,
                }
            })
            .collect::<Vec<DeudaIndice>>(),
    );
    if let Ok(mut cache) = estado.cache_deudas.lock() {
        *cache = Some((version, Arc::clone(&lista)));
    }
    Ok(lista)
}

fn rango_fechas_preset(
    preset: Option<&str>,
    desde: Option<i64>,
    hasta: Option<i64>,
) -> (Option<i64>, Option<i64>) {
    use chrono::{FixedOffset, TimeZone, Utc};
    let tz = FixedOffset::east_opt(-4 * 3600).unwrap();
    let ahora = Utc::now().with_timezone(&tz);
    let Some(medianoche) = ahora.date_naive().and_hms_opt(0, 0, 0) else {
        return (desde, hasta);
    };
    let inicio_hoy = tz
        .from_local_datetime(&medianoche)
        .single()
        .map(|d| d.timestamp())
        .unwrap_or(0);
    let fin_hoy = inicio_hoy + 86_400 - 1;
    match preset.unwrap_or("") {
        "hoy" => (Some(inicio_hoy), Some(fin_hoy)),
        "3dias" => (Some(inicio_hoy - 2 * 86_400), Some(fin_hoy)),
        "semana" => (Some(inicio_hoy - 6 * 86_400), Some(fin_hoy)),
        "mes" => (Some(inicio_hoy - 29 * 86_400), Some(fin_hoy)),
        "rango" => (desde, hasta),
        "fecha" => (desde, desde.map(|d| d + 86_400 - 1)),
        _ => (None, None),
    }
}

/// Busca deudas (abiertas y liquidadas) por nombre, con filtros de situacion y
/// de fecha. Usa un indice en memoria que se invalida cuando cambia una deuda,
/// para no recorrer el arbol en cada tecla ni bloquear la caja.
#[tauri::command]
fn buscar_deudas(
    estado: tauri::State<AppState>,
    texto: Option<String>,
    situacion: Option<String>,
    campo_fecha: Option<String>,
    preset: Option<String>,
    desde_unix: Option<i64>,
    hasta_unix: Option<i64>,
    pagina: Option<u32>,
) -> Result<BusquedaDeudasDto, UIError> {
    config_requerida(&estado)?;
    let indice = armar_indice_deudas(&estado)?;
    let q = normalizar_busqueda(texto.as_deref().unwrap_or(""));
    let situacion = situacion.unwrap_or_default();
    let por_cierre = campo_fecha.as_deref() == Some("liquidacion");
    let (desde, hasta) = rango_fechas_preset(preset.as_deref(), desde_unix, hasta_unix);

    let filtradas: Vec<&DeudaIndice> = indice
        .iter()
        .filter(|d| {
            if !q.is_empty() && !d.normalizado.contains(&q) && !d.cliente_normalizado.contains(&q) {
                return false;
            }
            match situacion.as_str() {
                "abierta" => {
                    if !d.abierta {
                        return false;
                    }
                }
                "liquidada" => {
                    if d.abierta {
                        return false;
                    }
                }
                _ => {}
            }
            let fecha = if por_cierre {
                d.cierre_unix
            } else {
                d.apertura_unix
            };
            if let Some(desde) = desde {
                if fecha < desde {
                    return false;
                }
            }
            if let Some(hasta) = hasta {
                if fecha == 0 || fecha > hasta {
                    return false;
                }
            }
            true
        })
        .collect();

    let total_deudas = filtradas.len() as u32;
    let mut grupos: std::collections::BTreeMap<String, Vec<&DeudaIndice>> =
        std::collections::BTreeMap::new();
    for d in &filtradas {
        grupos.entry(d.normalizado.clone()).or_default().push(d);
    }

    let mut armados: Vec<DeudorGrupoDto> = grupos
        .into_values()
        .map(|mut deudas| {
            deudas.sort_by(|a, b| {
                b.abierta.cmp(&a.abierta).then_with(|| {
                    let fa = if a.cierre_unix > 0 {
                        a.cierre_unix
                    } else {
                        a.apertura_unix
                    };
                    let fb = if b.cierre_unix > 0 {
                        b.cierre_unix
                    } else {
                        b.apertura_unix
                    };
                    fb.cmp(&fa)
                })
            });
            let nombre = deudas.first().map(|d| d.nombre.clone()).unwrap_or_default();
            let abiertas = deudas.iter().filter(|d| d.abierta).count() as u32;
            let saldo = deudas
                .iter()
                .filter(|d| d.abierta)
                .fold(Decimal::ZERO, |acc, d| acc + d.saldo_usd);
            let historico = deudas
                .iter()
                .fold(Decimal::ZERO, |acc, d| acc + d.total_usd);
            let ultima = deudas
                .iter()
                .map(|d| {
                    if d.cierre_unix > 0 {
                        d.cierre_unix
                    } else {
                        d.apertura_unix
                    }
                })
                .max()
                .unwrap_or(0);
            DeudorGrupoDto {
                nombre,
                abiertas,
                saldo_usd: saldo.to_string(),
                historico_usd: historico.to_string(),
                total_deudas: deudas.len() as u32,
                ultima_unix: ultima,
                deudas: deudas
                    .iter()
                    .map(|d| DeudaItemDto {
                        id: d.id.clone(),
                        nombre: d.nombre.clone(),
                        abierta: d.abierta,
                        apertura_unix: d.apertura_unix,
                        cierre_unix: if d.cierre_unix > 0 {
                            Some(d.cierre_unix)
                        } else {
                            None
                        },
                        total_usd: d.total_usd.to_string(),
                        abonos_usd: d.abonos_usd.to_string(),
                        saldo_usd: d.saldo_usd.to_string(),
                    })
                    .collect(),
            }
        })
        .collect();

    armados.sort_by(|a, b| {
        b.abiertas
            .cmp(&a.abiertas)
            .then_with(|| b.ultima_unix.cmp(&a.ultima_unix))
            .then_with(|| a.nombre.cmp(&b.nombre))
    });

    let pagina = pagina.unwrap_or(1).max(1);
    let por_pagina: u32 = 50;
    let inicio = ((pagina - 1) * por_pagina) as usize;
    let total_grupos = armados.len() as u32;
    let grupos_pagina: Vec<DeudorGrupoDto> = armados
        .into_iter()
        .skip(inicio)
        .take(por_pagina as usize)
        .collect();

    Ok(BusquedaDeudasDto {
        total_grupos,
        total_deudas,
        pagina,
        por_pagina,
        grupos: grupos_pagina,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeudaLineaDto {
    sku: Sku,
    nombre: Nombre,
    cantidad: String,
    precio_usd: String,
    subtotal_usd: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeudaDetalleDto {
    id: String,
    nombre: String,
    abierta: bool,
    apertura_unix: i64,
    cierre_unix: Option<i64>,
    total_usd: String,
    abonos_usd: String,
    abonos_bs: String,
    saldo_usd: String,
    nota: Option<String>,
    lineas: Vec<DeudaLineaDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pagos: Option<Vec<PagoTicketDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    estado_vuelto: Option<String>,
}

/// Detalle de lectura de un registro (venta directa, cuenta o deuda) con sus
/// consumos exactos, abonos y, si fue liquidado, los pagos del cierre.
#[tauri::command]
fn detalle_deuda(
    estado: tauri::State<AppState>,
    venta_id: String,
) -> Result<DeudaDetalleDto, UIError> {
    config_requerida(&estado)?;
    let venta = con_ledger(&estado, |db| db.cargar_venta(&venta_id))?
        .ok_or_else(|| UIError::new("registro inexistente", "No se encontro el registro"))?;
    let abierta = venta.es_cuenta_abierta && venta.estado == EstadoVenta::Abierta;
    let total = if abierta {
        venta.lineas.total_usd()
    } else {
        venta.total_usd
    };
    let abonos = venta.abonos_usd.unwrap_or(Decimal::ZERO);
    let abonos_bs = venta.abonos_bs.unwrap_or(Decimal::ZERO);
    let saldo = if abierta {
        let pendiente = total - abonos;
        if pendiente > Decimal::ZERO {
            pendiente
        } else {
            Decimal::ZERO
        }
    } else {
        Decimal::ZERO
    };
    let lineas = (0..venta.lineas.skus.len())
        .map(|i| DeudaLineaDto {
            sku: venta.lineas.skus[i].clone(),
            nombre: venta.lineas.nombres[i].clone(),
            cantidad: venta.lineas.cantidades[i].to_string(),
            precio_usd: venta.lineas.precios_usd[i].to_string(),
            subtotal_usd: (venta.lineas.cantidades[i] * venta.lineas.precios_usd[i]).to_string(),
        })
        .collect();
    let pagos = if venta.pagos.is_empty() {
        None
    } else {
        Some(
            venta
                .pagos
                .iter()
                .map(|p| PagoTicketDto {
                    metodo: p.metodo.clone(),
                    moneda: Some(p.moneda.clone()),
                    monto_usd: p.monto_usd.to_string(),
                    monto_bs: p.monto_bs.to_string(),
                    tasa_cambio: p.tasa_cambio.map(|t| t.to_string()),
                    referencia: p.referencia.clone(),
                })
                .collect(),
        )
    };
    Ok(DeudaDetalleDto {
        id: venta.id,
        nombre: venta.etiqueta,
        abierta,
        apertura_unix: venta.fecha_apertura_unix,
        cierre_unix: if venta.fecha_cierre_unix > 0 {
            Some(venta.fecha_cierre_unix)
        } else {
            None
        },
        total_usd: total.to_string(),
        abonos_usd: abonos.to_string(),
        abonos_bs: abonos_bs.to_string(),
        saldo_usd: saldo.to_string(),
        nota: venta.nota,
        lineas,
        pagos,
        estado_vuelto: venta.estado_vuelto,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HistorialItemDto {
    id: String,
    canal: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    etiqueta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    operador: Option<String>,
    abierta: bool,
    apertura_unix: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cierre_unix: Option<i64>,
    total_usd: String,
    total_bs: String,
    metodos: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResumenDeudorDto {
    abiertas: u32,
    saldo_usd: String,
    historico_usd: String,
    total_deudas: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BusquedaHistorialDto {
    total: u32,
    pagina: u32,
    por_pagina: u32,
    limite_alcanzado: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    resumen_deudor: Option<ResumenDeudorDto>,
    items: Vec<HistorialItemDto>,
}

fn canal_de_venta(venta: &Venta) -> &'static str {
    let abierta = venta.es_cuenta_abierta && venta.estado == EstadoVenta::Abierta;
    if abierta && venta.tipo == "deuda" {
        "deuda_abierta"
    } else if venta.tipo == "deuda" {
        "liquidacion_deuda"
    } else if venta.tipo == "saldo_a_favor" {
        "saldo_a_favor"
    } else if venta.es_cuenta_abierta && abierta {
        "cuenta_abierta"
    } else if venta.es_cuenta_abierta {
        "cuenta_liquidada"
    } else {
        "venta_directa"
    }
}

fn operador_por_fecha(rangos: &[(i64, i64, String)], fecha: i64) -> Option<String> {
    if fecha <= 0 || rangos.is_empty() {
        return None;
    }
    let idx = rangos.partition_point(|(inicio, _, _)| *inicio <= fecha);
    if idx == 0 {
        return None;
    }
    let (_, fin, operador) = &rangos[idx - 1];
    if fecha <= *fin {
        Some(operador.clone())
    } else {
        None
    }
}

/// Busca en el historial de ventas con paginacion de memoria acotada: recorre
/// el arbol una vez sin materializarlo, cuenta coincidencias y conserva solo
/// los `offset + pagina` registros mas recientes (monton acotado). Solo lectura.
#[tauri::command]
fn buscar_historial(
    estado: tauri::State<AppState>,
    texto: Option<String>,
    tipo: Option<String>,
    campo_fecha: Option<String>,
    preset: Option<String>,
    desde_unix: Option<i64>,
    hasta_unix: Option<i64>,
    operador: Option<String>,
    metodo: Option<String>,
    pagina: Option<u32>,
) -> Result<BusquedaHistorialDto, UIError> {
    config_requerida(&estado)?;
    let texto_crudo: String = texto.unwrap_or_default().chars().take(40).collect();
    let q = normalizar_busqueda(&texto_crudo);
    let tipo = tipo.unwrap_or_default();
    let por_cierre = campo_fecha.as_deref() == Some("liquidacion");
    let (desde, hasta) = rango_fechas_preset(preset.as_deref(), desde_unix, hasta_unix);
    let operador_buscado = operador.unwrap_or_default().trim().to_string();
    let metodo_buscado = metodo.unwrap_or_default().trim().to_string();
    const TOPE_PAGINAS: u32 = 200;
    let pagina_solicitada = pagina.unwrap_or(1).max(1);
    let pagina = pagina_solicitada.min(TOPE_PAGINAS);
    let limite_alcanzado = pagina_solicitada > TOPE_PAGINAS;
    let por_pagina: u32 = 50;
    let cap = (((pagina - 1) * por_pagina) + por_pagina) as usize;

    let jornadas = con_ledger(&estado, |db| db.listar_jornadas(usize::MAX))?;
    let mut rangos: Vec<(i64, i64, String)> = jornadas
        .iter()
        .map(|j| {
            (
                j.inicio_unix,
                j.fin_unix.unwrap_or(i64::MAX),
                j.operador_actual.clone(),
            )
        })
        .collect();
    rangos.sort_by_key(|(inicio, _, _)| *inicio);

    let mut contador: u32 = 0;
    let mut seleccion: Vec<(i64, String, HistorialItemDto)> = Vec::new();
    // Resumen del deudor para busquedas por nombre (memoria O(1)).
    let mut res_abiertas: u32 = 0;
    let mut res_total: u32 = 0;
    let mut res_saldo = Decimal::ZERO;
    let mut res_historico = Decimal::ZERO;

    con_ledger(&estado, |db| {
        db.por_cada_venta(|venta| {
            let abierta = venta.es_cuenta_abierta && venta.estado == EstadoVenta::Abierta;
            let canal = canal_de_venta(&venta);
            let coincide_tipo = match tipo.as_str() {
                "" | "todo" => true,
                "directa" => canal == "venta_directa",
                "cuenta" => canal == "cuenta_abierta" || canal == "cuenta_liquidada",
                "cuenta_abierta" => canal == "cuenta_abierta",
                "cuenta_liquidada" => canal == "cuenta_liquidada",
                "liquidacion" => canal == "liquidacion_deuda",
                "saldo" => canal == "saldo_a_favor",
                _ => true,
            };
            if !coincide_tipo {
                return;
            }
            let fecha_filtro = if por_cierre {
                venta.fecha_cierre_unix
            } else {
                venta.fecha_apertura_unix
            };
            if por_cierre && fecha_filtro <= 0 {
                return;
            }
            if let Some(desde) = desde {
                if fecha_filtro < desde {
                    return;
                }
            }
            if let Some(hasta) = hasta {
                if fecha_filtro == 0 || fecha_filtro > hasta {
                    return;
                }
            }
            let fecha_operador = if venta.fecha_cierre_unix > 0 {
                venta.fecha_cierre_unix
            } else {
                venta.fecha_apertura_unix
            };
            let operador_venta = operador_por_fecha(&rangos, fecha_operador);
            if !operador_buscado.is_empty()
                && operador_venta.as_deref() != Some(operador_buscado.as_str())
            {
                return;
            }
            if !metodo_buscado.is_empty() && !venta.pagos.iter().any(|p| p.metodo == metodo_buscado)
            {
                return;
            }
            if !q.is_empty() {
                let en_nombre = normalizar_busqueda(&venta.etiqueta).contains(&q);
                let en_operador = operador_venta
                    .as_deref()
                    .map(|o| normalizar_busqueda(o).contains(&q))
                    .unwrap_or(false);
                let en_id = venta.id.to_lowercase().contains(&q);
                if !(en_nombre || en_operador || en_id) {
                    return;
                }
            }
            contador += 1;
            let total = if abierta {
                venta.lineas.total_usd()
            } else {
                venta.total_usd
            };
            let total_bs = if abierta {
                venta.lineas.total_bs()
            } else {
                venta.total_bs
            };
            let es_deuda_resumen = canal == "deuda_abierta" || canal == "liquidacion_deuda";
            if es_deuda_resumen {
                res_total += 1;
                res_historico += total;
                if abierta {
                    res_abiertas += 1;
                    let abonos = venta.abonos_usd.unwrap_or(Decimal::ZERO);
                    let pendiente = total - abonos;
                    res_saldo += if pendiente > Decimal::ZERO {
                        pendiente
                    } else {
                        Decimal::ZERO
                    };
                }
            }
            let item = HistorialItemDto {
                id: venta.id.clone(),
                canal: canal.to_string(),
                etiqueta: if venta.etiqueta.trim().is_empty() {
                    None
                } else {
                    Some(venta.etiqueta.clone())
                },
                operador: operador_venta.clone(),
                abierta,
                apertura_unix: venta.fecha_apertura_unix,
                cierre_unix: if venta.fecha_cierre_unix > 0 {
                    Some(venta.fecha_cierre_unix)
                } else {
                    None
                },
                total_usd: total.to_string(),
                total_bs: total_bs.to_string(),
                metodos: venta.pagos.iter().map(|p| p.metodo.clone()).collect(),
            };
            seleccion.push((fecha_operador, venta.id.clone(), item));
            if seleccion.len() > cap {
                if let Some((pos, _)) = seleccion
                    .iter()
                    .enumerate()
                    .min_by(|a, b| a.1.0.cmp(&b.1.0).then_with(|| a.1.1.cmp(&b.1.1)))
                {
                    seleccion.remove(pos);
                }
            }
        })
    })?;

    seleccion.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    let inicio = ((pagina - 1) * por_pagina) as usize;
    let items: Vec<HistorialItemDto> = seleccion
        .into_iter()
        .skip(inicio)
        .take(por_pagina as usize)
        .map(|(_, _, item)| item)
        .collect();

    let resumen_deudor = if !q.is_empty()
        && res_total > 0
        && matches!(tipo.as_str(), "" | "todo" | "deuda" | "liquidacion")
    {
        Some(ResumenDeudorDto {
            abiertas: res_abiertas,
            saldo_usd: res_saldo.to_string(),
            historico_usd: res_historico.to_string(),
            total_deudas: res_total,
        })
    } else {
        None
    };

    Ok(BusquedaHistorialDto {
        total: contador,
        pagina,
        por_pagina,
        limite_alcanzado,
        resumen_deudor,
        items,
    })
}

// ---------------- comandos: historico tasas ----------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RegistroHistoricoTasa {
    id: String,
    valor: String,
    fecha_hora: String,
    tipo: String,
}

#[tauri::command]
fn listar_historico_tasas(
    estado: tauri::State<AppState>,
) -> Result<Vec<RegistroHistoricoTasa>, UIError> {
    config_requerida(&estado)?;
    let tasas = con_ledger(&estado, |db| db.ultimas_tasas(50))?;
    Ok(tasas
        .iter()
        .enumerate()
        .map(|(i, t)| RegistroHistoricoTasa {
            id: format!("tasa-{}", i),
            valor: t.valor_bs_por_usd.to_string(),
            fecha_hora: fecha_vet_str(t.fecha_unix, "%Y-%m-%d %H:%M"),
            tipo: {
                let fuente_len = t.fuente_len as usize;
                let fuente_str = std::str::from_utf8(&t.fuente[..fuente_len]).unwrap_or("BCV");
                if fuente_str == "MANUAL" {
                    "manual".to_string()
                } else {
                    "automatico".to_string()
                }
            },
        })
        .collect())
}

// ---------------- comandos: respaldos (aliases) ----------------

#[tauri::command]
fn crear_respaldo(estado: tauri::State<AppState>) -> Result<RespaldoInfo, UIError> {
    let dir = get_backup_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| UIError::new("error directorio", &e.to_string()))?;
    let ruta = std::path::Path::new(&dir).join(format!("datio_manual_{}.backup", ahora_unix()));
    let ruta_str = ruta.to_string_lossy().to_string();
    let payload = BackupRuta {
        ruta: ruta_str,
        licencia: None,
    };
    let meta = exportar_backup(estado, payload)?;
    let archivo_nombre = ruta
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let id = archivo_nombre.replace(".backup", "");
    let fecha = fecha_vet_str(meta.timestamp_unix, "%Y-%m-%d %H:%M");
    let tamano_kb = std::fs::metadata(&ruta)
        .map(|m| m.len() / 1024)
        .unwrap_or(0);
    Ok(RespaldoInfo {
        id,
        fecha,
        archivo_nombre,
        registros: meta.total_registros,
        tamano_kb,
        checksum_sha256: meta.checksum_sha256,
    })
}

#[tauri::command]
fn listar_respaldos(estado: tauri::State<AppState>) -> Result<Vec<RespaldoInfo>, UIError> {
    let dir = get_backup_dir()?;
    let ledger = estado
        .ledger
        .lock()
        .map_err(|e| UIError::new("lock", &e.to_string()))?;
    let metas = ledger
        .listar_backups(&dir)
        .map_err(|e| UIError::new("error listando respaldos", &e.to_string()))?;
    let mut infos = Vec::new();
    for (archivo_nombre, meta) in metas {
        let fecha = fecha_vet_str(meta.timestamp_unix, "%Y-%m-%d %H:%M");
        let ruta = std::path::Path::new(&dir).join(&archivo_nombre);
        let id = archivo_nombre.replace(".backup", "");
        let tamano_kb = std::fs::metadata(&ruta)
            .map(|m| m.len() / 1024)
            .unwrap_or(0);
        infos.push(RespaldoInfo {
            id,
            fecha,
            archivo_nombre,
            registros: meta.total_registros,
            tamano_kb,
            checksum_sha256: meta.checksum_sha256,
        });
    }
    infos.sort_by(|a, b| b.fecha.cmp(&a.fecha));
    Ok(infos)
}

#[tauri::command]
fn restaurar_desde_respaldo(
    state: tauri::State<AppState>,
    archivo: Option<String>,
    licencia: Option<String>,
) -> Result<bool, UIError> {
    if con_ledger(&state, |db| db.jornada_actual())?.is_some() {
        return Err(UIError::new(
            "cierre la jornada",
            "Cierre la jornada actual antes de restaurar un respaldo",
        ));
    }
    let ruta = match archivo
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
    {
        Some(a) => a,
        None => {
            // Sin archivo explicito se restaura el respaldo mas reciente real
            // (antes se inventaba una ruta por timestamp que nunca existia).
            let dir = get_backup_dir()?;
            let nombre = con_ledger(&state, |db| db.listar_backups(&dir))?
                .into_iter()
                .next()
                .map(|(nombre, _)| nombre)
                .ok_or_else(|| {
                    UIError::new(
                        "sin respaldos",
                        "No hay respaldos disponibles para restaurar",
                    )
                })?;
            std::path::Path::new(&dir)
                .join(nombre)
                .to_string_lossy()
                .to_string()
        }
    };
    let payload = BackupRuta { ruta, licencia };
    importar_backup(state, payload).map(|_| true)
}

#[tauri::command]
fn restaurar_desde_archivo(
    state: tauri::State<AppState>,
    contenido_base64: String,
    nombre_archivo: String,
    licencia: Option<String>,
) -> Result<BackupMetadataDto, UIError> {
    if con_ledger(&state, |db| db.jornada_actual())?.is_some() {
        return Err(UIError::new(
            "cierre la jornada",
            "Cierre la jornada actual antes de restaurar un respaldo",
        ));
    }
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&contenido_base64)
        .map_err(|e| UIError::new("archivo corrupto", &e.to_string()))?;
    let dir = get_backup_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| UIError::new("error directorio", &e.to_string()))?;
    let safe_name = nombre_archivo.replace(['/', '\\', '\0'], "_");
    let ruta = std::path::Path::new(&dir).join(&safe_name);
    std::fs::write(&ruta, &bytes)
        .map_err(|e| UIError::new("error escribiendo archivo", &e.to_string()))?;
    let payload = BackupRuta {
        ruta: ruta.to_string_lossy().to_string(),
        licencia,
    };
    let meta = importar_backup(state, payload)?;
    Ok(BackupMetadataDto {
        version: meta.version,
        timestamp_unix: meta.timestamp_unix,
        arboles: meta.arboles,
        total_registros: meta.total_registros,
        checksum_sha256: meta.checksum_sha256,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let mut db_path = std::path::PathBuf::from("../datiolabs_db");
            if let Ok(dir) = app.path().app_data_dir() {
                let _ = std::fs::create_dir_all(&dir);
                db_path = dir.join("datiolabs_db");
            }
            let ledger = Ledger::abrir(db_path.to_str().unwrap_or("../datiolabs_db"))
                .map_err(UIError::from)?;

            let mut ruta_cache = db_path.clone();
            ruta_cache.set_file_name("tasa_bcv_cache.json");

            let ledger_arc = Arc::new(Mutex::new(ledger));
            let servicio_tasa = Arc::new(ServicioTasa::new(ruta_cache, ledger_arc.clone())?);

            let app_state = AppState {
                ledger: ledger_arc.clone(),
                servicio_tasa: servicio_tasa.clone(),
                dedup_ventas: Arc::new(std::sync::Mutex::new(HashMap::new())),
                ultimo_latido: Arc::new(std::sync::atomic::AtomicI64::new(0)),
                cache_deudas: Arc::new(std::sync::Mutex::new(None)),
            };

            app.manage(app_state.clone());

            iniciar_refresco(servicio_tasa.clone());

            // Vigilante de interfaz: si el renderer de WebView2 se cuelga o se
            // cae, el latido del frontend se detiene y la ventana se recarga
            // sola en lugar de quedarse en blanco. Con cooldown para no entrar
            // en bucle de recargas si el problema persiste.
            {
                use std::sync::atomic::Ordering;
                let ultimo = app_state.ultimo_latido.clone();
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    let mut ultimo_reload = 0i64;
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(10));
                        let ahora = ahora_unix();
                        let visto = ultimo.load(Ordering::Relaxed);
                        if visto > 0 && ahora - visto > 30 && ahora - ultimo_reload > 120 {
                            if let Some(ventana) = handle.get_webview_window("main") {
                                let _ = ventana.reload();
                            }
                            ultimo.store(ahora, Ordering::Relaxed);
                            ultimo_reload = ahora;
                        }
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            obtener_config,
            inicializar_negocio,
            validar_pin_dueno,
            obtener_licencia,
            validar_licencia,
            generar_licencia,
            crear_producto,
            listar_productos,
            compra_stock,
            registrar_merma,
            crear_lote,
            listar_lotes,
            registrar_venta,
            previsualizar_cobro,
            abrir_cuenta,
            listar_cuentas,
            agregar_consumo,
            cerrar_cuenta,
            previsualizar_cierre_cuenta,
            datos_panel,
            obtener_tasa_bcv,
            forzar_actualizacion_tasa,
            fijar_tasa_manual,
            volver_a_bcv,
            exportar_backup,
            importar_backup,
            listar_backups,
            auto_backup,
            get_backup_dir,
            listar_categorias,
            crear_categoria,
            eliminar_categoria,
            listar_tasas_impuestos,
            crear_tasa_impuesto,
            eliminar_tasa_impuesto,
            listar_metodos_pago,
            crear_metodo_pago,
            eliminar_metodo_pago,
            listar_operadores,
            crear_operador,
            editar_operador,
            eliminar_operador,
            alternar_operador,
            obtener_semaforo_stock,
            guardar_semaforo_stock,
            cambiar_pin_dueno,
            actualizar_producto,
            eliminar_producto,
            reducir_stock,
            obtener_jornada_actual,
            buscar_deudas,
            detalle_deuda,
            buscar_historial,
            latido,
            listar_historico_jornadas,
            abrir_jornada,
            asignar_operadores_turno,
            relevar_operador_jornada,
            cerrar_jornada,
            eliminar_consumo,
            abonar_cuenta,
            editar_abono_cuenta,
            editar_cuenta,
            convertir_cuenta_a_deuda,
            listar_ventas,
            listar_anulaciones,
            listar_redondeos,
            eliminar_venta,
            listar_historico_tasas,
            crear_respaldo,
            listar_respaldos,
            restaurar_desde_respaldo,
            restaurar_desde_archivo
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            eprintln!("Error fatal: {}", e);
        });
}
