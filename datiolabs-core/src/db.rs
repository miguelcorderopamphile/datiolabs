use crate::capacidades::ErrorNegocio;
use crate::models::{
    Anulacion, Catalogo, Categoria, ConfigNegocio, EventoTasaBcv, Jornada, MetodoPagoConfig,
    MovimientoStock, Operador, Producto, Redondeo, SemaforoStock, TasaImpuesto, Venta,
};
use crate::modulos::panaderia::{LibroLotes, Lote};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sled::Db;
use std::sync::Arc;

/// Version del conjunto de cuentas/deudas. Cambia con cada escritura que las
/// afecta (alta, consumo, abono, cierre, anulacion o restauracion), para que el
/// buscador invalide su indice en memoria sin recorrer el arbol en cada tecla.
pub static VERSION_CUENTAS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Serialize, Deserialize)]
pub struct BackupHeader {
    pub version: u32,
    pub timestamp_unix: i64,
    pub arboles_count: u32,
}

const ARBOL_TASAS: &str = "tasas_bcv";
const ARBOL_PRODUCTOS: &str = "productos";
const ARBOL_VENTAS: &str = "ventas";
const ARBOL_MOVIMIENTOS: &str = "movimientos";
const ARBOL_LOTES: &str = "lotes";
const ARBOL_CONFIG: &str = "config";
const ARBOL_SESIONES: &str = "sesiones";
const ARBOL_CATEGORIAS: &str = "categorias";
const ARBOL_TASAS_IMPUESTOS: &str = "tasas_impuestos";
const ARBOL_METODOS_PAGO: &str = "metodos_pago";
const ARBOL_OPERADORES: &str = "operadores";
const ARBOL_JORNADAS: &str = "jornadas";
const ARBOL_SEMAFORO: &str = "semaforo_stock";
const ARBOL_ANULACIONES: &str = "anulaciones";
const ARBOL_REDONDEOS: &str = "redondeos";
const CLAVE_CONFIG: &[u8] = b"config";

const TODOS_ARBOLES: &[&str] = &[
    ARBOL_TASAS,
    ARBOL_PRODUCTOS,
    ARBOL_VENTAS,
    ARBOL_MOVIMIENTOS,
    ARBOL_LOTES,
    ARBOL_CONFIG,
    ARBOL_SESIONES,
    ARBOL_CATEGORIAS,
    ARBOL_TASAS_IMPUESTOS,
    ARBOL_METODOS_PAGO,
    ARBOL_OPERADORES,
    ARBOL_JORNADAS,
    ARBOL_SEMAFORO,
    ARBOL_ANULACIONES,
    ARBOL_REDONDEOS,
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupMetadata {
    pub version: u32,
    pub timestamp_unix: i64,
    pub arboles: Vec<String>,
    pub total_registros: usize,
    pub checksum_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupData {
    pub metadata: BackupMetadata,
    pub arboles: Vec<ArbolBackup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbolBackup {
    pub nombre: String,
    pub registros: Vec<RegistroBackup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistroBackup {
    pub clave: Vec<u8>,
    pub valor: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum DbError {
    #[error("fallo del almacenamiento embebido: {0}")]
    Almacenamiento(#[from] sled::Error),
    #[error("fallo de serializacion binaria: {0}")]
    Serializacion(#[from] bincode::Error),
    #[error("violacion de negocio: {0}")]
    Negocio(#[from] ErrorNegocio),
    #[error("error de E/S: {0}")]
    Io(#[from] std::io::Error),
}

pub struct Database {
    db: Arc<Db>,
}

impl Database {
    pub fn abrir(path: &str) -> Result<Self, DbError> {
        let db = sled::open(path)?;
        let ledger = Self { db: Arc::new(db) };
        ledger.semilla_si_vacia()?;
        Ok(ledger)
    }

    fn semilla_si_vacia(&self) -> Result<(), DbError> {
        let tree = self.db.open_tree(ARBOL_TASAS_IMPUESTOS)?;
        // Solo se siembra en estado de fabrica (sin config): si el usuario
        // borra todas las tasas despues de inicializar, no resucitan.
        if tree.is_empty() && self.cargar_config()?.is_none() {
            let defaults = vec![
                TasaImpuesto {
                    id: "IVA-0".into(),
                    nombre: "Exento".into(),
                    porcentaje: Decimal::ZERO,
                },
                TasaImpuesto {
                    id: "IVA-16".into(),
                    nombre: "Regular (16%)".into(),
                    porcentaje: dec!(16),
                },
                TasaImpuesto {
                    id: "IVA-8".into(),
                    nombre: "Bajo (8%)".into(),
                    porcentaje: dec!(8),
                },
            ];
            for t in &defaults {
                tree.insert(t.id.as_bytes(), bincode::serialize(t)?)?;
            }
        }
        Ok(())
    }

    pub fn inner_db(&self) -> Arc<Db> {
        self.db.clone()
    }

    // ---------------- tasas BCV ----------------

    /// Persists a BCV rate event under a chronological big-endian key and
    /// couples it with its SHA-256 signature before any write occurs.
    pub fn insertar_tasa(&self, mut tasa: EventoTasaBcv) -> Result<EventoTasaBcv, DbError> {
        tasa.firma_sha256 = firmar_tasa(&tasa);
        let tree = self.db.open_tree(ARBOL_TASAS)?;
        // Clave con la firma incluida: dos tasas en el mismo segundo no se pisan.
        let clave = clave_cronologica(tasa.fecha_unix, tasa.firma_sha256.as_bytes());
        tree.insert(clave, bincode::serialize(&tasa)?)?;
        Ok(tasa)
    }

    pub fn ultimas_tasas(&self, cantidad: usize) -> Result<Vec<EventoTasaBcv>, DbError> {
        let tree = self.db.open_tree(ARBOL_TASAS)?;
        let mut tasas = Vec::with_capacity(cantidad.min(1024));
        for par in tree.iter().rev().take(cantidad) {
            let (_, valor) = par?;
            tasas.push(bincode::deserialize(&valor)?);
        }
        Ok(tasas)
    }

    // ---------------- configuracion (arranque en blanco) ----------------

    /// Ausencia de registro = fabrica. El wizard escribe exactamente una vez.
    pub fn guardar_config(&self, config: &ConfigNegocio) -> Result<(), DbError> {
        if self.cargar_config()?.is_some() {
            return Err(DbError::Negocio(ErrorNegocio::YaInicializado));
        }
        let tree = self.db.open_tree(ARBOL_CONFIG)?;
        tree.insert(CLAVE_CONFIG, bincode::serialize(config)?)?;
        Ok(())
    }

    pub fn cargar_config(&self) -> Result<Option<ConfigNegocio>, DbError> {
        let tree = self.db.open_tree(ARBOL_CONFIG)?;
        match tree.get(CLAVE_CONFIG)? {
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
            None => Ok(None),
        }
    }

    /// Actualiza la configuracion existente. Rechaza si no existe.
    pub fn actualizar_config(&self, config: &ConfigNegocio) -> Result<(), DbError> {
        if self.cargar_config()?.is_none() {
            return Err(DbError::Negocio(ErrorNegocio::YaInicializado));
        }
        let tree = self.db.open_tree(ARBOL_CONFIG)?;
        tree.insert(CLAVE_CONFIG, bincode::serialize(config)?)?;
        Ok(())
    }

    // ---------------- categorias ----------------

    pub fn listar_categorias(&self) -> Result<Vec<Categoria>, DbError> {
        let tree = self.db.open_tree(ARBOL_CATEGORIAS)?;
        let mut cats = Vec::new();
        for par in tree.iter() {
            let (_, v) = par?;
            cats.push(bincode::deserialize(&v)?);
        }
        Ok(cats)
    }

    pub fn guardar_categoria(&self, cat: &Categoria) -> Result<(), DbError> {
        let tree = self.db.open_tree(ARBOL_CATEGORIAS)?;
        tree.insert(cat.id.as_bytes(), bincode::serialize(cat)?)?;
        Ok(())
    }

    pub fn eliminar_categoria(&self, id: &str) -> Result<(), DbError> {
        let tree = self.db.open_tree(ARBOL_CATEGORIAS)?;
        tree.remove(id.as_bytes())?;
        Ok(())
    }

    // ---------------- tasas impuestos ----------------

    pub fn listar_tasas_impuestos(&self) -> Result<Vec<TasaImpuesto>, DbError> {
        let tree = self.db.open_tree(ARBOL_TASAS_IMPUESTOS)?;
        let mut tasas = Vec::new();
        for par in tree.iter() {
            let (_, v) = par?;
            tasas.push(bincode::deserialize(&v)?);
        }
        Ok(tasas)
    }

    pub fn guardar_tasa_impuesto(&self, t: &TasaImpuesto) -> Result<(), DbError> {
        let tree = self.db.open_tree(ARBOL_TASAS_IMPUESTOS)?;
        tree.insert(t.id.as_bytes(), bincode::serialize(t)?)?;
        Ok(())
    }

    pub fn eliminar_tasa_impuesto(&self, id: &str) -> Result<(), DbError> {
        let tree = self.db.open_tree(ARBOL_TASAS_IMPUESTOS)?;
        tree.remove(id.as_bytes())?;
        Ok(())
    }

    // ---------------- metodos de pago ----------------

    pub fn listar_metodos_pago(&self) -> Result<Vec<MetodoPagoConfig>, DbError> {
        let tree = self.db.open_tree(ARBOL_METODOS_PAGO)?;
        let mut metodos = Vec::new();
        for par in tree.iter() {
            let (_, v) = par?;
            metodos.push(bincode::deserialize(&v)?);
        }
        Ok(metodos)
    }

    pub fn guardar_metodo_pago(&self, m: &MetodoPagoConfig) -> Result<(), DbError> {
        let tree = self.db.open_tree(ARBOL_METODOS_PAGO)?;
        tree.insert(m.nombre.as_bytes(), bincode::serialize(m)?)?;
        Ok(())
    }

    pub fn eliminar_metodo_pago(&self, nombre: &str) -> Result<(), DbError> {
        let tree = self.db.open_tree(ARBOL_METODOS_PAGO)?;
        tree.remove(nombre.as_bytes())?;
        Ok(())
    }

    // ---------------- operadores ----------------

    pub fn listar_operadores(&self) -> Result<Vec<Operador>, DbError> {
        let tree = self.db.open_tree(ARBOL_OPERADORES)?;
        let mut ops = Vec::new();
        for par in tree.iter() {
            let (_, v) = par?;
            ops.push(bincode::deserialize(&v)?);
        }
        Ok(ops)
    }

    pub fn guardar_operador(&self, op: &Operador) -> Result<(), DbError> {
        let tree = self.db.open_tree(ARBOL_OPERADORES)?;
        tree.insert(op.id.as_bytes(), bincode::serialize(op)?)?;
        Ok(())
    }

    pub fn eliminar_operador(&self, id: &str) -> Result<(), DbError> {
        let tree = self.db.open_tree(ARBOL_OPERADORES)?;
        tree.remove(id.as_bytes())?;
        Ok(())
    }

    // ---------------- jornadas ----------------

    pub fn jornada_actual(&self) -> Result<Option<Jornada>, DbError> {
        let tree = self.db.open_tree(ARBOL_JORNADAS)?;
        let mut abiertas: Vec<Jornada> = Vec::new();
        for par in tree.iter() {
            let (_, v) = par?;
            let j: Jornada = bincode::deserialize(&v)?;
            if j.estado == "abierta" {
                abiertas.push(j);
            }
        }
        // La jornada vigente es la de inicio mas reciente, no la de id mayor.
        abiertas.sort_by_key(|a| std::cmp::Reverse(a.inicio_unix));
        Ok(abiertas.into_iter().next())
    }

    pub fn listar_jornadas(&self, limite: usize) -> Result<Vec<Jornada>, DbError> {
        let tree = self.db.open_tree(ARBOL_JORNADAS)?;
        let mut jornadas: Vec<Jornada> = Vec::new();
        for par in tree.iter() {
            let (_, v) = par?;
            jornadas.push(bincode::deserialize(&v)?);
        }
        jornadas.sort_by_key(|a| std::cmp::Reverse(a.inicio_unix));
        jornadas.truncate(limite);
        Ok(jornadas)
    }

    pub fn guardar_jornada(&self, j: &Jornada) -> Result<(), DbError> {
        let tree = self.db.open_tree(ARBOL_JORNADAS)?;
        tree.insert(j.id.as_bytes(), bincode::serialize(j)?)?;
        Ok(())
    }

    // ---------------- semaforo stock ----------------

    pub fn cargar_semaforo(&self) -> Result<Option<SemaforoStock>, DbError> {
        let tree = self.db.open_tree(ARBOL_SEMAFORO)?;
        match tree.get(b"semaforo")? {
            Some(v) => Ok(Some(bincode::deserialize(&v)?)),
            None => Ok(None),
        }
    }

    pub fn guardar_semaforo(&self, s: &SemaforoStock) -> Result<(), DbError> {
        let tree = self.db.open_tree(ARBOL_SEMAFORO)?;
        tree.insert(b"semaforo", bincode::serialize(s)?)?;
        Ok(())
    }

    // ---------------- productos ----------------

    pub fn guardar_producto(&self, producto: &Producto) -> Result<(), DbError> {
        let tree = self.db.open_tree(ARBOL_PRODUCTOS)?;
        if tree.contains_key(producto.sku.as_bytes())? {
            return Err(DbError::Negocio(ErrorNegocio::SkuDuplicado(
                producto.sku.as_str().to_string(),
            )));
        }
        tree.insert(producto.sku.as_bytes(), bincode::serialize(producto)?)?;
        Ok(())
    }

    pub fn actualizar_producto(&self, producto: &Producto) -> Result<(), DbError> {
        let tree = self.db.open_tree(ARBOL_PRODUCTOS)?;
        tree.insert(producto.sku.as_bytes(), bincode::serialize(producto)?)?;
        Ok(())
    }

    /// Carga fila a fila desde almacenamiento y proyecta a columnas contiguas
    /// para el runtime (DoD: cold row-wise, hot column-wise).
    pub fn cargar_catalogo(&self) -> Result<Catalogo, DbError> {
        let tree = self.db.open_tree(ARBOL_PRODUCTOS)?;
        let mut filas = Vec::new();
        for par in tree.iter() {
            let (_, valor) = par?;
            filas.push(bincode::deserialize::<Producto>(&valor)?);
        }
        Ok(Catalogo::desde_productos(filas))
    }

    /// Movimiento append-only + actualizacion atomica de la columna stock.
    /// La firma se acopla antes del primer write (inmutabilidad forense).
    pub fn aplicar_movimiento(
        &self,
        mut movimiento: MovimientoStock,
        delta_catalogo: impl FnOnce(&str, Decimal),
    ) -> Result<MovimientoStock, DbError> {
        // Validar ANTES de escribir: sin movimientos huerfanos por SKU
        // inexistente ni por intentos de dejar stock negativo.
        let tree = self.db.open_tree(ARBOL_PRODUCTOS)?;
        let bytes = tree
            .get(movimiento.sku.as_bytes())?
            .ok_or(DbError::Negocio(ErrorNegocio::ProductoInexistente))?;
        let mut producto: Producto = bincode::deserialize(&bytes)?;
        let nuevo = producto.stock + movimiento.delta;
        if nuevo < Decimal::ZERO {
            return Err(DbError::Negocio(ErrorNegocio::StockInsuficiente {
                disponible: producto.stock,
                solicitado: -movimiento.delta,
            }));
        }

        movimiento.firma_sha256 = firmar_movimiento(&movimiento);
        let clave_mov = clave_cronologica(movimiento.fecha_unix, movimiento.id.as_bytes());
        self.db
            .open_tree(ARBOL_MOVIMIENTOS)?
            .insert(clave_mov, bincode::serialize(&movimiento)?)?;

        producto.stock = nuevo;
        tree.insert(movimiento.sku.as_bytes(), bincode::serialize(&producto)?)?;

        delta_catalogo(&movimiento.sku, movimiento.delta);
        self.db.flush()?;
        Ok(movimiento)
    }

    // ---------------- lotes de panaderia ----------------

    pub fn guardar_lote(&self, lote: &Lote) -> Result<(), DbError> {
        let tree = self.db.open_tree(ARBOL_LOTES)?;
        tree.insert(lote.id.as_bytes(), bincode::serialize(lote)?)?;
        Ok(())
    }

    pub fn cargar_lotes(&self) -> Result<LibroLotes, DbError> {
        let tree = self.db.open_tree(ARBOL_LOTES)?;
        let mut libro = LibroLotes::nuevo();
        for par in tree.iter() {
            let (_, valor) = par?;
            libro.insertar(bincode::deserialize::<Lote>(&valor)?);
        }
        Ok(libro)
    }

    /// Lista completa de lotes (para gestion FEFO en inventario).
    pub fn listar_lotes(&self) -> Result<Vec<Lote>, DbError> {
        let tree = self.db.open_tree(ARBOL_LOTES)?;
        let mut salida = Vec::new();
        for par in tree.iter() {
            let (_, valor) = par?;
            salida.push(bincode::deserialize::<Lote>(&valor)?);
        }
        Ok(salida)
    }

    pub fn actualizar_disponible_lote(
        &self,
        lote_id: &str,
        disponible: Decimal,
    ) -> Result<(), DbError> {
        let tree = self.db.open_tree(ARBOL_LOTES)?;
        match tree.get(lote_id.as_bytes())? {
            Some(bytes) => {
                let mut lote: Lote = bincode::deserialize(&bytes)?;
                lote.disponible = disponible;
                tree.insert(lote_id.as_bytes(), bincode::serialize(&lote)?)?;
                Ok(())
            }
            None => Err(DbError::Negocio(ErrorNegocio::ProductoInexistente)),
        }
    }

    // ---------------- ventas ----------------

    /// Upsert de venta con firma acoplada. Las cuentas abiertas reescriben su
    /// registro al agregar lineas; el cierre congela totales y firma final.
    pub fn guardar_venta(&self, mut venta: Venta) -> Result<Venta, DbError> {
        venta.firma_sha256 = firmar_venta(&venta);
        let tree = self.db.open_tree(ARBOL_VENTAS)?;
        tree.insert(venta.id.as_bytes(), bincode::serialize(&venta)?)?;
        self.db.flush()?;
        if venta.es_cuenta_abierta || venta.tipo == "deuda" {
            VERSION_CUENTAS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(venta)
    }

    /// Recorre las ventas de a una, sin materializar el arbol completo.
    /// La memoria del consumidor es su responsabilidad: permite busquedas
    /// paginadas de memoria acotada sin cargar todo el historico.
    pub fn por_cada_venta<F: FnMut(Venta)>(&self, mut consumidor: F) -> Result<(), DbError> {
        let tree = self.db.open_tree(ARBOL_VENTAS)?;
        for par in tree.iter() {
            let (_, valor) = par?;
            if let Ok(venta) = bincode::deserialize::<Venta>(&valor) {
                consumidor(venta);
            }
        }
        Ok(())
    }

    pub fn cargar_venta(&self, venta_id: &str) -> Result<Option<Venta>, DbError> {
        let tree = self.db.open_tree(ARBOL_VENTAS)?;
        match tree.get(venta_id.as_bytes())? {
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
            None => Ok(None),
        }
    }

    /// Elimina una venta persistida por su ID y devuelve la venta eliminada si existía.
    pub fn eliminar_venta(&self, venta_id: &str) -> Result<Option<Venta>, DbError> {
        let tree = self.db.open_tree(ARBOL_VENTAS)?;
        let res: Result<Option<Venta>, DbError> = match tree.remove(venta_id.as_bytes())? {
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
            None => Ok(None),
        };
        self.db.flush()?;
        if let Ok(Some(ref venta)) = res
            && (venta.es_cuenta_abierta || venta.tipo == "deuda")
        {
            VERSION_CUENTAS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        res
    }

    /// Libro de anulaciones: guarda la venta original firmada para auditoria.
    pub fn guardar_anulacion(&self, anulacion: &Anulacion) -> Result<(), DbError> {
        let mut firmada = anulacion.clone();
        let bytes = bincode::serialize(&firmada)?;
        firmada.firma_sha256 = format!("{:x}", Sha256::digest(&bytes));
        let tree = self.db.open_tree(ARBOL_ANULACIONES)?;
        let clave = clave_cronologica(firmada.fecha_anulacion_unix, firmada.id.as_bytes());
        tree.insert(clave, bincode::serialize(&firmada)?)?;
        self.db.flush()?;
        Ok(())
    }

    /// Libro de redondeos voluntarios: ingreso auditado por venta y turno.
    pub fn guardar_redondeo(&self, redondeo: &Redondeo) -> Result<(), DbError> {
        let mut firmado = redondeo.clone();
        let bytes = bincode::serialize(&firmado)?;
        firmado.firma_sha256 = format!("{:x}", Sha256::digest(&bytes));
        let tree = self.db.open_tree(ARBOL_REDONDEOS)?;
        let clave = clave_cronologica(firmado.fecha_unix, firmado.id.as_bytes());
        tree.insert(clave, bincode::serialize(&firmado)?)?;
        self.db.flush()?;
        Ok(())
    }

    /// Listado cronologico descendente de redondeos.
    pub fn listar_redondeos(&self, limite: usize) -> Result<Vec<Redondeo>, DbError> {
        let tree = self.db.open_tree(ARBOL_REDONDEOS)?;
        let mut salida = Vec::new();
        for par in tree.iter().rev().take(limite) {
            let (_, valor) = par?;
            salida.push(bincode::deserialize(&valor)?);
        }
        Ok(salida)
    }

    /// Listado cronologico descendente del libro de anulaciones.
    pub fn listar_anulaciones(&self, limite: usize) -> Result<Vec<Anulacion>, DbError> {
        let tree = self.db.open_tree(ARBOL_ANULACIONES)?;
        let mut salida = Vec::new();
        for par in tree.iter().rev().take(limite) {
            let (_, valor) = par?;
            salida.push(bincode::deserialize(&valor)?);
        }
        Ok(salida)
    }

    /// Escaneo cronologico descendente real (por fecha de cierre, no por id).
    pub fn ventas_recientes(&self, limite: usize) -> Result<Vec<Venta>, DbError> {
        let tree = self.db.open_tree(ARBOL_VENTAS)?;
        let mut ventas: Vec<Venta> = Vec::new();
        for par in tree.iter() {
            let (_, valor) = par?;
            ventas.push(bincode::deserialize(&valor)?);
        }
        ventas.sort_by(|a, b| {
            b.fecha_cierre_unix
                .cmp(&a.fecha_cierre_unix)
                .then_with(|| b.id.cmp(&a.id))
        });
        ventas.truncate(limite);
        Ok(ventas)
    }

    /// Consulta ventas dentro de un rango temporal exacto [desde_unix, hasta_unix].
    pub fn consultar_ventas_rango(
        &self,
        desde_unix: i64,
        hasta_unix: i64,
    ) -> Result<Vec<Venta>, DbError> {
        let tree = self.db.open_tree(ARBOL_VENTAS)?;
        let mut ventas = Vec::new();
        for par in tree.iter() {
            let (_, valor) = par?;
            let v: Venta = bincode::deserialize(&valor)?;
            if v.fecha_cierre_unix >= desde_unix && v.fecha_cierre_unix <= hasta_unix {
                ventas.push(v);
            }
        }
        ventas.sort_by_key(|v| v.fecha_cierre_unix);
        Ok(ventas)
    }

    /// Calcula metricas comerciales directas para un rango temporal.
    pub fn metricas_rango(
        &self,
        desde_unix: i64,
        hasta_unix: i64,
    ) -> Result<crate::analytics::MetricasPeriodo, DbError> {
        let ventas = self.consultar_ventas_rango(desde_unix, hasta_unix)?;
        let engine = crate::analytics::AnalyticsEngine::new();
        Ok(engine.calcular_metricas(&ventas))
    }

    pub fn cuentas_abiertas(&self) -> Result<Vec<Venta>, DbError> {
        Ok(self
            .ventas_recientes(usize::MAX)?
            .into_iter()
            .filter(|v| v.es_cuenta_abierta && v.estado == crate::models::EstadoVenta::Abierta)
            .collect())
    }

    pub fn exportar_backup(&self, ruta: &str) -> Result<BackupMetadata, DbError> {
        use sha2::{Digest, Sha256};
        use std::io::{BufWriter, Write};

        let file = std::fs::File::create(ruta)?;
        let mut writer = BufWriter::new(file);

        let mut total_registros = 0usize;
        let mut hasher = Sha256::new();

        let timestamp_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let mut metadata = BackupMetadata {
            version: 1,
            timestamp_unix,
            arboles: TODOS_ARBOLES.iter().map(|s| s.to_string()).collect(),
            total_registros: 0,
            checksum_sha256: String::new(),
        };

        let header = BackupHeader {
            version: 1,
            timestamp_unix,
            arboles_count: TODOS_ARBOLES.len() as u32,
        };
        let header_bytes = bincode::serialize(&header)?;
        hasher.update(&header_bytes);
        writer.write_all(&(header_bytes.len() as u32).to_le_bytes())?;
        writer.write_all(&header_bytes)?;

        for nombre_arbol in TODOS_ARBOLES {
            let tree = self.db.open_tree(nombre_arbol)?;
            let nombre_bytes = nombre_arbol.as_bytes();
            hasher.update(nombre_bytes);
            writer.write_all(&(nombre_bytes.len() as u32).to_le_bytes())?;
            writer.write_all(nombre_bytes)?;

            let entries: Vec<(sled::IVec, sled::IVec)> =
                tree.iter().filter_map(|r| r.ok()).collect();
            let n_entries = entries.len() as u32;
            hasher.update(n_entries.to_le_bytes());
            writer.write_all(&n_entries.to_le_bytes())?;

            for (clave, valor) in entries {
                hasher.update(&clave);
                hasher.update(&valor);

                writer.write_all(&(clave.len() as u32).to_le_bytes())?;
                writer.write_all(&clave)?;
                writer.write_all(&(valor.len() as u32).to_le_bytes())?;
                writer.write_all(&valor)?;
                total_registros += 1;
            }
        }

        writer.flush()?;

        let checksum = format!("{:x}", hasher.finalize());
        metadata.total_registros = total_registros;
        metadata.checksum_sha256 = checksum;

        let metadata_bytes = bincode::serialize(&metadata)?;
        let mut file = std::fs::OpenOptions::new().write(true).open(ruta)?;
        use std::io::Seek;
        file.seek(std::io::SeekFrom::End(0))?;
        file.write_all(&metadata_bytes)?;
        file.flush()?;

        Ok(metadata)
    }

    pub fn importar_backup(
        &self,
        ruta: &str,
        licencia_esperada: &str,
    ) -> Result<BackupMetadata, DbError> {
        use sha2::{Digest, Sha256};
        use std::io::{BufReader, Read, Seek, SeekFrom};

        // Restaurar reemplaza el conjunto completo: invalida el indice de deudas.
        VERSION_CUENTAS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let file = std::fs::File::open(ruta)?;
        let mut reader = BufReader::new(file);
        let file_size = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;

        let mut hasher = Sha256::new();

        let mut len_buf = [0u8; 4];
        reader.read_exact(&mut len_buf)?;
        let header_len = u32::from_le_bytes(len_buf) as usize;
        let mut header_buf = vec![0u8; header_len];
        reader.read_exact(&mut header_buf)?;
        hasher.update(&header_buf);

        let _header: BackupHeader = bincode::deserialize(&header_buf)?;

        let mut _total_registros = 0usize;
        #[allow(clippy::type_complexity)]
        let mut datos_a_restaurar: Vec<(String, Vec<(Vec<u8>, Vec<u8>)>)> = Vec::new();
        let mut licencia_en_respaldo = String::new();

        for _ in 0.._header.arboles_count {
            reader.read_exact(&mut len_buf)?;
            let nombre_len = u32::from_le_bytes(len_buf) as usize;
            let mut nombre_buf = vec![0u8; nombre_len];
            reader.read_exact(&mut nombre_buf)?;
            hasher.update(&nombre_buf);
            let nombre_arbol = std::str::from_utf8(&nombre_buf)
                .map_err(|_| {
                    DbError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Nombre árbol inválido",
                    ))
                })?
                .to_string();

            reader.read_exact(&mut len_buf)?;
            let cant_registros = u32::from_le_bytes(len_buf);
            hasher.update(len_buf);

            let mut registros_arbol = Vec::with_capacity(cant_registros as usize);

            for _ in 0..cant_registros {
                reader.read_exact(&mut len_buf)?;
                let clave_len = u32::from_le_bytes(len_buf) as usize;
                let mut clave_buf = vec![0u8; clave_len];
                reader.read_exact(&mut clave_buf)?;
                hasher.update(&clave_buf);

                reader.read_exact(&mut len_buf)?;
                let valor_len = u32::from_le_bytes(len_buf) as usize;
                let mut valor_buf = vec![0u8; valor_len];
                reader.read_exact(&mut valor_buf)?;
                hasher.update(&valor_buf);

                if nombre_arbol == ARBOL_CONFIG
                    && clave_buf.as_slice() == CLAVE_CONFIG
                    && let Ok(cfg) = bincode::deserialize::<ConfigNegocio>(&valor_buf)
                {
                    licencia_en_respaldo = cfg.licencia_clave.clone();
                }

                registros_arbol.push((clave_buf, valor_buf));
                _total_registros += 1;
            }
            datos_a_restaurar.push((nombre_arbol, registros_arbol));
        }

        let metadata_len = file_size - reader.stream_position()?;
        let mut metadata_buf = vec![0u8; metadata_len as usize];
        reader.read_exact(&mut metadata_buf)?;

        let metadata: BackupMetadata = bincode::deserialize(&metadata_buf)?;

        let checksum = format!("{:x}", hasher.finalize());
        if checksum != metadata.checksum_sha256 {
            return Err(DbError::Serializacion(bincode::Error::from(
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Checksum de backup inválido",
                ),
            )));
        }

        // Si el respaldo contiene una clave de licencia activa/registrada,
        // validar obligatoriamente que la clave provista coincida de forma exacta.
        let lic_respaldo_limpia = licencia_en_respaldo.trim().replace('-', "");
        let lic_esperada_limpia = licencia_esperada.trim().replace('-', "");

        if !lic_respaldo_limpia.is_empty()
            && (lic_esperada_limpia.is_empty() || lic_esperada_limpia != lic_respaldo_limpia)
        {
            return Err(DbError::Negocio(ErrorNegocio::LicenciaInvalida));
        }

        // Una vez validada la integridad SHA-256 y la clave de licencia, vaciar y poblar árboles
        let nombres_presentes: Vec<String> = datos_a_restaurar
            .iter()
            .map(|(nombre, _)| nombre.clone())
            .collect();
        for (nombre_arbol, registros) in datos_a_restaurar {
            let tree = self.db.open_tree(&nombre_arbol)?;
            tree.clear()?;
            for (clave, valor) in registros {
                tree.insert(clave, valor)?;
            }
        }

        // Un respaldo viejo no trae los arboles nuevos (p.ej. anulaciones):
        // se limpian para no dejar residuos de la base anterior.
        for conocido in TODOS_ARBOLES {
            if !nombres_presentes.iter().any(|n| n == conocido) {
                self.db.open_tree(conocido)?.clear()?;
            }
        }

        self.db.flush()?;
        Ok(metadata)
    }

    /// Lista los respaldos del directorio con su nombre real de archivo.
    /// El nombre se conserva porque `crear_respaldo` usa el prefijo
    /// `datio_manual_` y reconstruirlo por timestamp daba rutas inexistentes.
    pub fn listar_backups(
        &self,
        directorio: &str,
    ) -> Result<Vec<(String, BackupMetadata)>, DbError> {
        use std::io::{BufReader, Read, Seek, SeekFrom};
        let mut metadatos: Vec<(String, BackupMetadata)> = Vec::new();
        for entry in std::fs::read_dir(directorio)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("backup") {
                let nombre_archivo = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string();
                let file = std::fs::File::open(&path)?;
                let mut reader = BufReader::new(file);
                let file_size = reader.seek(SeekFrom::End(0))?;
                reader.seek(SeekFrom::Start(0))?;

                let mut len_buf = [0u8; 4];
                if reader.read_exact(&mut len_buf).is_err() {
                    continue;
                }
                let header_len = u32::from_le_bytes(len_buf) as usize;
                let mut header_buf = vec![0u8; header_len];
                if reader.read_exact(&mut header_buf).is_err() {
                    continue;
                }
                let _header: BackupHeader = match bincode::deserialize(&header_buf) {
                    Ok(h) => h,
                    Err(_) => continue,
                };

                let mut len_buf = [0u8; 4];
                let mut ok = true;
                for _ in 0.._header.arboles_count {
                    if reader.read_exact(&mut len_buf).is_err() {
                        ok = false;
                        break;
                    }
                    let nombre_len = u32::from_le_bytes(len_buf) as i64;
                    if reader.seek(SeekFrom::Current(nombre_len)).is_err() {
                        ok = false;
                        break;
                    }

                    if reader.read_exact(&mut len_buf).is_err() {
                        ok = false;
                        break;
                    }
                    let cant_registros = u32::from_le_bytes(len_buf);

                    for _ in 0..cant_registros {
                        if reader.read_exact(&mut len_buf).is_err() {
                            ok = false;
                            break;
                        }
                        let clave_len = u32::from_le_bytes(len_buf) as i64;
                        if reader.seek(SeekFrom::Current(clave_len)).is_err() {
                            ok = false;
                            break;
                        }

                        if reader.read_exact(&mut len_buf).is_err() {
                            ok = false;
                            break;
                        }
                        let valor_len = u32::from_le_bytes(len_buf) as i64;
                        if reader.seek(SeekFrom::Current(valor_len)).is_err() {
                            ok = false;
                            break;
                        }
                    }
                    if !ok {
                        break;
                    }
                }

                if ok {
                    let metadata_len = file_size - reader.stream_position()?;
                    if metadata_len > 0 && metadata_len < 1024 * 1024 {
                        let mut metadata_buf = vec![0u8; metadata_len as usize];
                        if reader.read_exact(&mut metadata_buf).is_ok()
                            && let Ok(metadata) =
                                bincode::deserialize::<BackupMetadata>(&metadata_buf)
                        {
                            metadatos.push((nombre_archivo, metadata));
                        }
                    }
                }
            }
        }
        metadatos.sort_by_key(|(_, b)| std::cmp::Reverse(b.timestamp_unix));
        Ok(metadatos)
    }

    pub fn auto_backup(
        &self,
        directorio: &str,
        max_backups: usize,
    ) -> Result<Option<BackupMetadata>, DbError> {
        let _ = std::fs::create_dir_all(directorio);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| DbError::Io(std::io::Error::other(e)))?;
        let timestamp = now.as_secs();
        let ruta = std::path::Path::new(directorio).join(format!("datio_{}.backup", timestamp));
        let ruta_str = ruta.to_str().ok_or_else(|| {
            DbError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ruta de backup no es UTF-8 valido",
            ))
        })?;

        let metadata = self.exportar_backup(ruta_str)?;

        // La rotacion automatica solo toca copias automaticas (`datio_`),
        // jamas los respaldos manuales del dueno (`datio_manual_`).
        let mut backups: Vec<(String, BackupMetadata)> = self
            .listar_backups(directorio)?
            .into_iter()
            .filter(|(nombre, _)| {
                nombre.starts_with("datio_") && !nombre.starts_with("datio_manual_")
            })
            .collect();
        // Al menos la copia recien creada sobrevive aunque max_backups sea 0.
        let permitidos = max_backups.max(1);
        if backups.len() > permitidos {
            for (nombre, _) in backups.drain(permitidos..) {
                let ruta_vieja = std::path::Path::new(directorio).join(nombre);
                let _ = std::fs::remove_file(ruta_vieja);
            }
        }

        Ok(Some(metadata))
    }
}

fn clave_cronologica(fecha_unix: i64, id: &[u8]) -> Vec<u8> {
    let mut clave = Vec::with_capacity(8 + id.len());
    clave.extend_from_slice(&(fecha_unix as u64).to_be_bytes());
    clave.extend_from_slice(id);
    clave
}

fn firmar_tasa(tasa: &EventoTasaBcv) -> String {
    let mut hasher = Sha256::new();
    hasher.update(tasa.valor_bs_por_usd.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(tasa.fecha_unix.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(&tasa.fuente[..tasa.fuente_len as usize]);
    format!("{:x}", hasher.finalize())
}

fn firmar_venta(venta: &Venta) -> String {
    let mut hasher = Sha256::new();
    hasher.update(venta.id.as_bytes());
    hasher.update(b"|");
    hasher.update(venta.lineas.total_usd().to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(venta.lineas.total_bs().to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(format!("{:?}", venta.estado).as_bytes());
    hasher.update(b"|");
    for i in 0..venta.lineas.skus.len() {
        hasher.update(venta.lineas.skus[i].as_bytes());
        hasher.update(venta.lineas.cantidades[i].to_string().as_bytes());
        hasher.update(venta.lineas.precios_usd[i].to_string().as_bytes());
        hasher.update(venta.lineas.tasas_bloqueadas[i].to_string().as_bytes());
        hasher.update(b"#");
    }
    format!("{:x}", hasher.finalize())
}

fn firmar_movimiento(mov: &MovimientoStock) -> String {
    let mut hasher = Sha256::new();
    hasher.update(mov.id.as_bytes());
    hasher.update(b"|");
    hasher.update(mov.sku.as_bytes());
    hasher.update(b"|");
    hasher.update(mov.delta.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(format!("{:?}", mov.motivo).as_bytes());
    hasher.update(b"|");
    hasher.update(mov.fecha_unix.to_string().as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capacidades::{CAP_UNITARIA, ErrorNegocio, RUBRO_LICORERIA};
    use crate::models::{EstadoVenta, MAX_FUENTE_LEN, MotivoMovimiento, Nombre, Sku};
    use rust_decimal_macros::dec;

    fn db_temporal(etiqueta: &str) -> Database {
        let dir = std::env::temp_dir().join(format!("datio_{}_{}", etiqueta, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Database::abrir(dir.to_str().unwrap()).unwrap()
    }

    fn producto_base(sku: &str) -> Producto {
        Producto {
            sku: Sku::new(sku).unwrap(),
            nombre: Nombre::new("Producto").unwrap(),
            precio_usd: dec!(2.50),
            impuesto_pct: dec!(16),
            stock: dec!(100),
            capacidades: CAP_UNITARIA,
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

    #[test]
    fn firma_es_determinista_y_sensible_al_contenido() {
        let mut fuente = [0u8; MAX_FUENTE_LEN];
        let fuente_str = "BCV";
        fuente[..fuente_str.len()].copy_from_slice(fuente_str.as_bytes());

        let tasa = EventoTasaBcv {
            valor_bs_por_usd: dec!(36.92),
            fecha_unix: 1_700_000_000,
            fuente,
            fuente_len: fuente_str.len() as u8,
            firma_sha256: String::new(),
        };
        let a = firmar_tasa(&tasa);
        assert_eq!(a.len(), 64);

        let mut venta = venta_simple("V1", dec!(100));
        let f1 = firmar_venta(&venta);
        venta.estado = EstadoVenta::Cerrada;
        let f2 = firmar_venta(&venta);
        assert_ne!(f1, f2);

        // Validar firma de movimiento contable
        let mov = MovimientoStock {
            id: "MOV-1".to_string(),
            sku: "SKU1".to_string(),
            delta: dec!(10),
            motivo: MotivoMovimiento::Compra,
            venta_id: None,
            fecha_unix: 1_700_000_000,
            firma_sha256: String::new(),
        };
        let f_mov = firmar_movimiento(&mov);
        assert_eq!(f_mov.len(), 64);

        // Validar clave cronologica determinista
        let c1 = clave_cronologica(1_700_000_000, b"ID-1");
        assert_eq!(c1.len(), 12);
    }

    fn venta_simple(id: &str, tasa: Decimal) -> crate::models::Venta {
        let mut lineas = crate::models::LineasVenta::nuevas();
        lineas.agregar(
            Sku::new("SKU1").unwrap(),
            Nombre::new("Uno").unwrap(),
            dec!(2),
            dec!(5),
            tasa,
            "unidad".to_string(),
        );
        crate::models::Venta {
            id: id.to_string(),
            etiqueta: String::new(),
            es_cuenta_abierta: false,
            estado: EstadoVenta::Abierta,
            lineas,
            tasa_del_dia: tasa,
            total_usd: dec!(10),
            total_bs: dec!(1000),
            monto_recibido_bs: Decimal::ZERO,
            vuelto_bs: Decimal::ZERO,
            pagos: Vec::new(),
            estado_vuelto: None,
            metodo_vuelto: None,
            monto_vuelto_usd: None,
            tasa_vuelto: None,
            fecha_apertura_unix: 1000,
            fecha_cierre_unix: 0,
            firma_sha256: String::new(),
            tipo: "venta".to_string(),
            cliente: None,
            nota: None,
            abonos_usd: None,
            abonos_bs: None,
        }
    }

    #[test]
    fn config_arranque_blanco_y_escritura_unica() {
        let db = db_temporal("cfg");
        assert!(db.cargar_config().unwrap().is_none());
        db.guardar_config(&ConfigNegocio {
            nombre: Nombre::new("Mi Licoreria").unwrap(),
            rubros: RUBRO_LICORERIA,
            pin_dueno_sha256: "abc".into(),
            licencia_clave: String::new(),
            licencia_titular: String::new(),
            licencia_estado: String::new(),
            privacidad_inventario: false,
        })
        .unwrap();
        let cfg = db.cargar_config().unwrap().unwrap();
        assert_eq!(cfg.nombre.as_str(), "Mi Licoreria");
        let duplicada = db.guardar_config(&ConfigNegocio {
            nombre: Nombre::new("Otra").unwrap(),
            rubros: 1,
            pin_dueno_sha256: String::new(),
            licencia_clave: String::new(),
            licencia_titular: String::new(),
            licencia_estado: String::new(),
            privacidad_inventario: false,
        });
        assert!(matches!(
            duplicada,
            Err(DbError::Negocio(ErrorNegocio::YaInicializado))
        ));
    }

    #[test]
    fn catalogo_roundtrip_y_stock_atomico_por_movimiento() {
        let db = db_temporal("cat");
        db.guardar_producto(&producto_base("A001")).unwrap();

        let mut catalogo = db.cargar_catalogo().unwrap();
        assert_eq!(catalogo.len(), 1);
        let idx = catalogo.indice_de("A001").unwrap();

        let mov = MovimientoStock {
            id: "M1".into(),
            sku: "A001".into(),
            delta: dec!(-3),
            motivo: MotivoMovimiento::Venta,
            venta_id: Some("V1".into()),
            fecha_unix: 5000,
            firma_sha256: String::new(),
        };
        let firmado = db
            .aplicar_movimiento(mov, |sku, delta| {
                if let Some(i) = catalogo.indice_de(sku) {
                    catalogo.aplicar_delta_stock(i, delta);
                }
            })
            .unwrap();
        assert!(!firmado.firma_sha256.is_empty());
        assert_eq!(catalogo.stock(idx), dec!(97));

        let recargado = db.cargar_catalogo().unwrap();
        let idx2 = recargado.indice_de("A001").unwrap();
        assert_eq!(recargado.stock(idx2), dec!(97));

        let fantasma = MovimientoStock {
            id: "M2".into(),
            sku: "NOPE".into(),
            delta: dec!(-1),
            motivo: MotivoMovimiento::Venta,
            venta_id: None,
            fecha_unix: 5001,
            firma_sha256: String::new(),
        };
        assert!(db.aplicar_movimiento(fantasma, |_, _| {}).is_err());
    }

    #[test]
    fn venta_upsert_y_filtro_cuentas_abiertas() {
        let db = db_temporal("ven");
        let mut abierta = venta_simple("CTA1", dec!(100));
        abierta.es_cuenta_abierta = true;
        abierta.etiqueta = "Mesa 3".into();
        db.guardar_venta(abierta.clone()).unwrap();

        abierta.lineas.agregar(
            Sku::new("SKU2").unwrap(),
            Nombre::new("Dos").unwrap(),
            dec!(1),
            dec!(7),
            dec!(101),
            "unidad".to_string(),
        );
        abierta.total_usd = abierta.lineas.total_usd();
        db.guardar_venta(abierta).unwrap();

        let guardada = db.cargar_venta("CTA1").unwrap().unwrap();
        assert_eq!(guardada.lineas.skus.len(), 2);
        assert_eq!(
            db.cuentas_abiertas().unwrap().len(),
            1,
            "upsert no debe duplicar cuentas"
        );

        let directa = venta_simple("V9", dec!(100));
        db.guardar_venta(directa).unwrap();
        assert_eq!(db.cuentas_abiertas().unwrap().len(), 1);
        assert_eq!(db.ventas_recientes(10).unwrap().len(), 2);
    }

    #[test]
    fn exportar_y_listar_backups() {
        let db = db_temporal("bck_test");
        let dir = std::env::temp_dir().join(format!("datio_bck_dir_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // Nombre real que usa crear_respaldo: el listado debe devolverlo tal cual.
        let backup_path = dir.join("datio_manual_123456.backup");
        let backup_path_str = backup_path.to_str().unwrap();

        let meta = db.exportar_backup(backup_path_str).unwrap();
        assert_eq!(meta.version, 1);

        let lista = db.listar_backups(dir.to_str().unwrap()).unwrap();
        assert_eq!(lista.len(), 1);
        assert_eq!(lista[0].0, "datio_manual_123456.backup");
        assert_eq!(lista[0].1.timestamp_unix, meta.timestamp_unix);
        assert_eq!(lista[0].1.checksum_sha256, meta.checksum_sha256);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_backup_no_borra_respaldos_manuales() {
        let db = db_temporal("bck_manual");
        let dir = std::env::temp_dir().join(format!("datio_bck_manual_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let manual = dir.join("datio_manual_1.backup");
        db.exportar_backup(manual.to_str().unwrap()).unwrap();

        for _ in 0..4 {
            db.auto_backup(dir.to_str().unwrap(), 2).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }

        assert!(
            manual.exists(),
            "el respaldo manual debe sobrevivir a la rotacion"
        );
        let autos = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with("datio_") && !n.starts_with("datio_manual_")
            })
            .count();
        assert!(autos <= 2, "automaticos rotados a 2, hay {autos}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exportar_y_restaurar_backup_con_presentaciones() {
        let db1 = db_temporal("bck_pres_1");
        let dir = std::env::temp_dir().join(format!("datio_pres_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let backup_path = dir.join("datio_pres.backup");
        let backup_path_str = backup_path.to_str().unwrap();

        // Producto con presentaciones múltiples y despiece
        let mut prod = Producto::nuevo_simple(
            Sku::new("BEBIDA-PACK").unwrap(),
            Nombre::new("Refresco 2L Pack 6").unwrap(),
            dec!(3.50),
            dec!(16),
            dec!(48),
            0,
        );
        prod.es_caja = true;
        prod.unidades_por_caja = Some(6);
        prod.precio_paquete_usd = Some(dec!(19.50));
        prod.nombre_paquete = Some("Caja 6 un.".to_string());
        prod.unidad = Some("un".to_string());
        prod.precio_bruto_usd = Some(dec!(2.20));
        prod.margen_pct = Some(dec!(35));

        db1.guardar_producto(&prod).unwrap();

        let meta_exp = db1.exportar_backup(backup_path_str).unwrap();
        assert!(meta_exp.total_registros > 0);

        let db2 = db_temporal("bck_pres_2");
        let meta_imp = db2.importar_backup(backup_path_str, "").unwrap();
        assert_eq!(meta_exp.checksum_sha256, meta_imp.checksum_sha256);

        let cat = db2.cargar_catalogo().unwrap();
        let idx = cat
            .indice_de("BEBIDA-PACK")
            .expect("Producto recuperado en catálogo");
        assert_eq!(cat.stock(idx), dec!(48));
        assert!(cat.es_caja(idx));
        assert_eq!(cat.unidades_por_caja(idx), Some(6));
        assert_eq!(cat.precio_paquete_usd(idx), Some(dec!(19.50)));
        assert_eq!(cat.nombre_paquete(idx), Some("Caja 6 un.".to_string()));
        assert_eq!(cat.unidad(idx), Some("un".to_string()));
        assert_eq!(cat.precio_bruto_usd(idx), Some(dec!(2.20)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restaurar_backup_exige_misma_licencia() {
        let db1 = db_temporal("bck_lic_1");
        let dir = std::env::temp_dir().join(format!("datio_lic_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let backup_path = dir.join("datio_lic.backup");
        let backup_path_str = backup_path.to_str().unwrap();

        let cfg = ConfigNegocio {
            nombre: Nombre::new("Abasto Don Pedro").unwrap(),
            rubros: 1,
            privacidad_inventario: true,
            pin_dueno_sha256: "".to_string(),
            licencia_clave: "0001-8888-1111-0000".to_string(),
            licencia_titular: "Don Pedro".to_string(),
            licencia_estado: "activa".to_string(),
        };
        db1.guardar_config(&cfg).unwrap();
        db1.exportar_backup(backup_path_str).unwrap();

        let db2 = db_temporal("bck_lic_2");
        // Con clave vacía o incorrecta debe fallar
        let err_vacio = db2.importar_backup(backup_path_str, "").unwrap_err();
        assert!(matches!(
            err_vacio,
            DbError::Negocio(ErrorNegocio::LicenciaInvalida)
        ));

        let err_malo = db2
            .importar_backup(backup_path_str, "9999-0000-0000-1111")
            .unwrap_err();
        assert!(matches!(
            err_malo,
            DbError::Negocio(ErrorNegocio::LicenciaInvalida)
        ));

        // Con la clave correcta (con o sin guiones) debe restaurar con éxito
        let meta_ok = db2
            .importar_backup(backup_path_str, "0001888811110000")
            .unwrap();
        assert!(meta_ok.total_registros > 0);

        let cfg_restaurada = db2.cargar_config().unwrap().expect("Config presente");
        assert_eq!(cfg_restaurada.licencia_clave, "0001-8888-1111-0000");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn eliminar_venta_persiste_y_desregistra() {
        let db = db_temporal("eliminar_venta_ledger");
        let venta_id = "VTA-TEST-001";
        let venta = venta_simple(venta_id, dec!(100));

        db.guardar_venta(venta).unwrap();
        let ventas_antes = db.ventas_recientes(10).unwrap();
        assert_eq!(ventas_antes.len(), 1);

        let eliminada = db.eliminar_venta(venta_id).unwrap();
        assert!(eliminada.is_some());
        assert_eq!(eliminada.unwrap().id, venta_id);

        let ventas_despues = db.ventas_recientes(10).unwrap();
        assert_eq!(ventas_despues.len(), 0);

        // Intentar eliminar nuevamente devuelve None
        let re_eliminada = db.eliminar_venta(venta_id).unwrap();
        assert!(re_eliminada.is_none());
    }

    #[test]
    fn crud_categorias_y_tasas_impuestos() {
        let db = db_temporal("cat_tasas");
        // Categorias
        let cat = Categoria {
            id: "CAT-BEB".to_string(),
            nombre: "Bebidas".to_string(),
        };
        db.guardar_categoria(&cat).unwrap();
        let cats = db.listar_categorias().unwrap();
        assert_eq!(cats.len(), 1);
        assert_eq!(cats[0].nombre, "Bebidas");
        db.eliminar_categoria("CAT-BEB").unwrap();
        assert_eq!(db.listar_categorias().unwrap().len(), 0);

        // Tasas impuestos: por semilla existen 3 ("IVA-0", "IVA-16", "IVA-8")
        let tasas_iniciales = db.listar_tasas_impuestos().unwrap();
        assert_eq!(tasas_iniciales.len(), 3);

        let tasa_imp = TasaImpuesto {
            id: "IVA-PERSONALIZADO".to_string(),
            nombre: "IVA Especial".to_string(),
            porcentaje: dec!(22.00),
        };
        db.guardar_tasa_impuesto(&tasa_imp).unwrap();
        let tasas = db.listar_tasas_impuestos().unwrap();
        assert_eq!(tasas.len(), 4);
        db.eliminar_tasa_impuesto("IVA-PERSONALIZADO").unwrap();
        assert_eq!(db.listar_tasas_impuestos().unwrap().len(), 3);
    }

    #[test]
    fn crud_metodos_pago_operadores_y_semaforo() {
        let db = db_temporal("mp_ops_sem");
        // Metodos de pago
        let mp = MetodoPagoConfig {
            nombre: "PUNTOD.VENTA".to_string(),
            moneda: "BS".to_string(),
        };
        db.guardar_metodo_pago(&mp).unwrap();
        let mps = db.listar_metodos_pago().unwrap();
        assert_eq!(mps.len(), 1);
        assert_eq!(mps[0].nombre, "PUNTOD.VENTA");
        db.eliminar_metodo_pago("PUNTOD.VENTA").unwrap();
        assert_eq!(db.listar_metodos_pago().unwrap().len(), 0);

        // Operadores
        let op = Operador {
            id: "OP-01".to_string(),
            nombre: "Elismar".to_string(),
            activo: true,
            creado_unix: 1000,
        };
        db.guardar_operador(&op).unwrap();
        let ops = db.listar_operadores().unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].nombre, "Elismar");
        db.eliminar_operador("OP-01").unwrap();
        assert_eq!(db.listar_operadores().unwrap().len(), 0);

        // Semaforo de stock
        assert!(db.cargar_semaforo().unwrap().is_none());
        let sem = SemaforoStock {
            rojo_max: 5,
            amarillo_max: 15,
        };
        db.guardar_semaforo(&sem).unwrap();
        let sem_cargado = db.cargar_semaforo().unwrap().unwrap();
        assert_eq!(sem_cargado.rojo_max, 5);
        assert_eq!(sem_cargado.amarillo_max, 15);
    }

    #[test]
    fn gestion_jornadas_y_metricas_rango() {
        let db = db_temporal("jor_met");
        assert!(db.jornada_actual().unwrap().is_none());

        let j1 = Jornada {
            id: "JOR-1".to_string(),
            inicio_unix: 1000,
            fin_unix: Some(2000),
            operador_inicial: "Luisin".to_string(),
            operador_actual: "Luisin".to_string(),
            operadores_relevo: vec!["Elismar".to_string()],
            operadores_activos: vec!["Luisin".to_string()],
            estado: "cerrada".to_string(),
            tasa_inicio: dec!(800),
            tasa_fin: Some(dec!(800)),
            ventas_total_usd: dec!(100),
            ventas_total_bs: dec!(80000),
            tickets_emitidos: 5,
            vuelto_pagado_bs: Decimal::ZERO,
            vuelto_retenido_bs: Decimal::ZERO,
            deudas_liquidadas_usd: Decimal::ZERO,
            entradas_stock_reg: 0,
            mermas_stock_reg: 0,
            cambios_precio_reg: 0,
            checksum_sha256: Some("sha-j1".to_string()),
        };
        db.guardar_jornada(&j1).unwrap();

        let j2 = Jornada {
            id: "JOR-2".to_string(),
            inicio_unix: 2001,
            fin_unix: None,
            operador_inicial: "Elismar".to_string(),
            operador_actual: "Elismar".to_string(),
            operadores_relevo: vec![],
            operadores_activos: vec!["Elismar".to_string(), "Luisin".to_string()],
            estado: "abierta".to_string(),
            tasa_inicio: dec!(800),
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
        db.guardar_jornada(&j2).unwrap();

        let activa = db
            .jornada_actual()
            .unwrap()
            .expect("Jornada activa encontrada");
        assert_eq!(activa.id, "JOR-2");
        assert_eq!(activa.operadores_activos.len(), 2);

        let todas = db.listar_jornadas(10).unwrap();
        assert_eq!(todas.len(), 2);

        // Métricas de ventas en rango
        let v1 = venta_simple("V-MET-1", dec!(800));
        db.guardar_venta(v1).unwrap();
        let rango = db.consultar_ventas_rango(0, 5000).unwrap();
        assert!(!rango.is_empty());
        let metricas = db.metricas_rango(0, 5000).unwrap();
        assert_eq!(metricas.total_usd, dec!(10));
        assert_eq!(metricas.total_bs, dec!(1000));
        assert_eq!(metricas.tickets, 1);
    }

    #[test]
    fn gestion_lotes_fefo_y_disponibilidad() {
        let db = db_temporal("lotes_libro");
        let lote1 = Lote {
            id: "LOT-001".to_string(),
            sku: "PAN-01".to_string(),
            cantidad_inicial: dec!(50),
            disponible: dec!(50),
            horneado_unix: 1000,
            caduce_unix: 2000,
        };
        db.guardar_lote(&lote1).unwrap();

        let libro = db.cargar_lotes().unwrap();
        assert_eq!(libro.len(), 1);
        assert_eq!(libro.lote_fefo("PAN-01", 1500), Some(0));

        // Actualizar disponible
        db.actualizar_disponible_lote("LOT-001", dec!(35)).unwrap();
        let libro_actualizado = db.cargar_lotes().unwrap();
        assert_eq!(libro_actualizado.len(), 1);
    }
}
