use crate::error::UIError;
use datiolabs_core::db::Database as Ledger;
use datiolabs_core::models::{EventoTasaBcv, MAX_FUENTE_LEN};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, LazyLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use scraper::{Html, Selector};

const URL_BCV: &str = "https://www.bcv.org.ve/";
const FUENTE: &str = "BCV";
const INTERVALO_REFRESCHO_SEG: u64 = 3600;
const TIMEOUT_HTTP_SEG: u64 = 30;

fn fuente_bytes(s: &str) -> ([u8; MAX_FUENTE_LEN], u8) {
    let mut buf = [0u8; MAX_FUENTE_LEN];
    let len = s.len().min(MAX_FUENTE_LEN);
    buf[..len].copy_from_slice(&s.as_bytes()[..len]);
    (buf, len as u8)
}

static SELECTOR_DIV: LazyLock<Option<Selector>> =
    LazyLock::new(|| Selector::parse("div.recuadrotsmc").ok());
static SELECTOR_SPAN: LazyLock<Option<Selector>> = LazyLock::new(|| Selector::parse("span").ok());
static SELECTOR_STRONG: LazyLock<Option<Selector>> =
    LazyLock::new(|| Selector::parse("strong").ok());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DireccionTasa {
    Subio,
    Bajo,
    Estable,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TasaInfo {
    pub valor: Decimal,
    pub fecha_unix: i64,
    pub fluctuacion_pct: Option<Decimal>,
    pub direccion: Option<DireccionTasa>,
    #[serde(default)]
    pub fuente: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheTasa {
    valor: Decimal,
    fecha_unix: i64,
    #[serde(default)]
    fuente: String,
}

#[derive(Default)]
struct Snapshot {
    actual: Option<CacheTasa>,
    anterior: Option<Decimal>,
}

pub struct ServicioTasa {
    cliente: reqwest::Client,
    ruta_cache: PathBuf,
    ledger: Arc<std::sync::Mutex<Ledger>>,
    snapshot: RwLock<Snapshot>,
}

impl ServicioTasa {
    pub fn new(
        ruta_cache: PathBuf,
        ledger: Arc<std::sync::Mutex<Ledger>>,
    ) -> Result<Self, UIError> {
        let cliente = reqwest::Client::builder()
            .timeout(Duration::from_secs(TIMEOUT_HTTP_SEG))
            .build()?;
        let actual = cargar_cache(&ruta_cache).or_else(|| {
            ledger
                .lock()
                .ok()
                .and_then(|db| db.ultimas_tasas(1).ok())
                .and_then(|tasas| tasas.into_iter().next())
                .map(|t| {
                    let fuente_len = t.fuente_len as usize;
                    let fuente_str = std::str::from_utf8(&t.fuente[..fuente_len])
                        .unwrap_or("BCV")
                        .to_string();
                    CacheTasa {
                        valor: t.valor_bs_por_usd,
                        fecha_unix: t.fecha_unix,
                        fuente: fuente_str,
                    }
                })
        });
        Ok(Self {
            cliente,
            ruta_cache,
            ledger,
            snapshot: RwLock::new(Snapshot {
                actual,
                anterior: None,
            }),
        })
    }

    pub fn info_actual(&self) -> TasaInfo {
        let guardia = self
            .snapshot
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let actual = match &guardia.actual {
            Some(a) => a,
            None => {
                return TasaInfo {
                    valor: Decimal::ZERO,
                    fecha_unix: 0,
                    fluctuacion_pct: None,
                    direccion: None,
                    fuente: Some("BCV".to_string()),
                };
            }
        };
        let fluctuacion_pct = calcular_fluctuacion(actual.valor, guardia.anterior);
        let direccion = fluctuacion_pct.map(direccion_desde_fluctuacion);
        TasaInfo {
            valor: actual.valor,
            fecha_unix: actual.fecha_unix,
            fluctuacion_pct,
            direccion,
            fuente: Some(if actual.fuente.is_empty() {
                "BCV".to_string()
            } else {
                actual.fuente.clone()
            }),
        }
    }

    /// La tasa aplicada es MANUAL: el refresco automatico del BCV no la toca.
    pub fn en_modo_manual(&self) -> bool {
        let guardia = self
            .snapshot
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guardia
            .actual
            .as_ref()
            .map(|a| a.fuente.eq_ignore_ascii_case("MANUAL"))
            .unwrap_or(false)
    }

    /// Refresco automatico: respeta la tasa manual (no la pisa).
    pub async fn refrescar(&self) -> Result<TasaInfo, UIError> {
        if self.en_modo_manual() {
            return Ok(self.info_actual());
        }
        self.refrescar_con_bcv(false).await
    }

    /// Accion explicita "volver a BCV": reemplaza la tasa manual por la oficial.
    pub async fn volver_a_bcv(&self) -> Result<TasaInfo, UIError> {
        self.refrescar_con_bcv(true).await
    }

    async fn refrescar_con_bcv(&self, permitir_override_manual: bool) -> Result<TasaInfo, UIError> {
        let valor = scrapear_bcv(&self.cliente).await?;
        if valor <= Decimal::ZERO || valor > Decimal::from(1_000_000) {
            return Err(UIError::new(
                format!("tasa BCV fuera de rango: {valor}"),
                "El portal devolvio un valor improbable; se conserva la tasa vigente",
            ));
        }
        let fecha_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default();

        {
            let mut guardia = self
                .snapshot
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Si el dueno fijo una tasa manual mientras el scrape estaba en
            // vuelo, el resultado del BCV se descarta: la manual gana. El
            // chequeo y la escritura ocurren bajo el mismo lock, de modo que
            // no existe ventana de carrera con establecer_tasa_manual.
            let manual_ahora = guardia
                .actual
                .as_ref()
                .map(|a| a.fuente.eq_ignore_ascii_case("MANUAL"))
                .unwrap_or(false);
            if manual_ahora && !permitir_override_manual {
                drop(guardia);
                return Ok(self.info_actual());
            }
            let anterior = guardia.actual.take().map(|a| a.valor);
            guardia.actual = Some(CacheTasa {
                valor,
                fecha_unix,
                fuente: FUENTE.to_string(),
            });
            guardia.anterior = anterior;
        }

        if let Some(db) = self.ledger.lock().ok().as_deref() {
            let (fuente_arr, fuente_len) = fuente_bytes(FUENTE);
            let _ = db.insertar_tasa(EventoTasaBcv {
                valor_bs_por_usd: valor,
                fecha_unix,
                fuente: fuente_arr,
                fuente_len,
                firma_sha256: String::new(),
            });
        }

        guardar_cache(
            &self.ruta_cache,
            &CacheTasa {
                valor,
                fecha_unix,
                fuente: FUENTE.to_string(),
            },
        );

        Ok(self.info_actual())
    }

    pub fn establecer_tasa_manual(&self, valor: Decimal) -> Result<TasaInfo, UIError> {
        if valor <= Decimal::ZERO {
            return Err(UIError::new(
                "tasa inválida",
                "La tasa debe ser mayor a cero",
            ));
        }
        let fecha_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default();

        if let Some(db) = self.ledger.lock().ok().as_deref() {
            let (fuente_arr, fuente_len) = fuente_bytes("MANUAL");
            let _ = db.insertar_tasa(EventoTasaBcv {
                valor_bs_por_usd: valor,
                fecha_unix,
                fuente: fuente_arr,
                fuente_len,
                firma_sha256: String::new(),
            });
        }

        let cache = CacheTasa {
            valor,
            fecha_unix,
            fuente: "MANUAL".to_string(),
        };
        guardar_cache(&self.ruta_cache, &cache);

        {
            let mut guardia = self
                .snapshot
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let anterior = guardia.actual.take().map(|a| a.valor);
            guardia.actual = Some(cache);
            guardia.anterior = anterior;
        }

        Ok(self.info_actual())
    }
}

pub fn iniciar_refresco(servicio: Arc<ServicioTasa>) {
    tauri::async_runtime::spawn(async move {
        loop {
            let _ = servicio.refrescar().await;
            tokio::time::sleep(Duration::from_secs(INTERVALO_REFRESCHO_SEG)).await;
        }
    });
}

async fn scrapear_bcv(cliente: &reqwest::Client) -> Result<Decimal, UIError> {
    let respuesta = cliente.get(URL_BCV).send().await?.error_for_status()?;
    let html = respuesta.text().await?;
    extraer_valor_usd(&html).ok_or(UIError {
        message: "BCV sin valor USD legible".to_string(),
        diagnosis: "La estructura del portal BCV no expuso el recuadro USD; se conserva la ultima tasa cacheada".to_string(),
    })
}

fn extraer_valor_usd(html: &str) -> Option<Decimal> {
    let documento = Html::parse_document(html);
    let div_sel = SELECTOR_DIV.as_ref()?;
    let span_sel = SELECTOR_SPAN.as_ref()?;
    let strong_sel = SELECTOR_STRONG.as_ref()?;

    for div in documento.select(div_sel) {
        let es_usd = div
            .select(span_sel)
            .any(|s| s.text().collect::<String>().trim() == "USD");
        if !es_usd {
            continue;
        }
        if let Some(strong) = div.select(strong_sel).next() {
            let texto = strong.text().collect::<String>();
            if let Some(valor) = normalizar_valor_bcv(&texto) {
                return Some(valor);
            }
        }
    }
    None
}

/// Normaliza el valor publicado por el BCV sin asumir un unico separador:
/// - "1.234,56" (miles con punto, decimal con coma) -> 1234.56
/// - "36,92" -> 36.92
/// - "36.92" -> 36.92
fn normalizar_valor_bcv(texto: &str) -> Option<Decimal> {
    let limpio: String = texto
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == ',' || *c == '.')
        .collect();
    if limpio.is_empty() {
        return None;
    }
    let normalizado = if limpio.contains(',') && limpio.contains('.') {
        limpio.replace('.', "").replace(',', ".")
    } else {
        limpio.replace(',', ".")
    };
    Decimal::from_str(&normalizado)
        .ok()
        .filter(|v| *v > Decimal::ZERO)
}

fn calcular_fluctuacion(actual: Decimal, anterior: Option<Decimal>) -> Option<Decimal> {
    let previa = anterior.filter(|p| !p.is_zero())?;
    Some(((actual - previa) / previa) * Decimal::from(100))
}

fn direccion_desde_fluctuacion(fluctuacion: Decimal) -> DireccionTasa {
    if fluctuacion > Decimal::ZERO {
        DireccionTasa::Subio
    } else if fluctuacion < Decimal::ZERO {
        DireccionTasa::Bajo
    } else {
        DireccionTasa::Estable
    }
}

fn cargar_cache(ruta: &PathBuf) -> Option<CacheTasa> {
    let crudo = std::fs::read_to_string(ruta).ok()?;
    serde_json::from_str(&crudo).ok()
}

fn guardar_cache(ruta: &PathBuf, cache: &CacheTasa) {
    if let Ok(json) = serde_json::to_string(cache) {
        let _ = std::fs::write(ruta, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    const HTML_BCV: &str = r#"
        <html><body>
            <div class="recuadrotsmc">
                <span>EUR</span><strong> 100,00000000</strong>
            </div>
            <div class="recuadrotsmc">
                <span>USD</span><strong> 36,92000000</strong>
            </div>
        </body></html>"#;

    #[test]
    fn extrae_valor_usd_y_no_eur() {
        assert_eq!(extraer_valor_usd(HTML_BCV), Some(dec!(36.92)));
    }

    #[test]
    fn html_sin_recuadro_devuelve_none() {
        assert_eq!(extraer_valor_usd("<html><body></body></html>"), None);
    }

    #[test]
    fn fluctuacion_sube_baja_y_cero_divisor() {
        assert_eq!(
            calcular_fluctuacion(dec!(50.00), Some(dec!(40.00))),
            Some(dec!(25.00))
        );
        assert_eq!(
            calcular_fluctuacion(dec!(30.00), Some(dec!(40.00))),
            Some(dec!(-25.00))
        );
        assert_eq!(
            direccion_desde_fluctuacion(dec!(25.00)),
            DireccionTasa::Subio
        );
        assert_eq!(
            direccion_desde_fluctuacion(dec!(-0.50)),
            DireccionTasa::Bajo
        );
        assert_eq!(
            direccion_desde_fluctuacion(Decimal::ZERO),
            DireccionTasa::Estable
        );
        assert_eq!(calcular_fluctuacion(dec!(40.00), Some(Decimal::ZERO)), None);
        assert_eq!(calcular_fluctuacion(dec!(40.00), None), None);
    }

    #[test]
    fn normaliza_formatos_de_miles_y_decimales() {
        assert_eq!(normalizar_valor_bcv("36,92000000"), Some(dec!(36.92)));
        assert_eq!(normalizar_valor_bcv("1.234,56"), Some(dec!(1234.56)));
        assert_eq!(normalizar_valor_bcv("36.92"), Some(dec!(36.92)));
        assert_eq!(normalizar_valor_bcv("  USD  "), None);
        assert_eq!(normalizar_valor_bcv("0,00"), None);
    }

    #[tokio::test]
    async fn tasa_manual_es_pegajosa_frente_al_refresco() {
        let dir = std::env::temp_dir().join(format!("datio_tasa_manual_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let ledger = Arc::new(std::sync::Mutex::new(
            Ledger::abrir(dir.join("db").to_str().unwrap()).expect("db"),
        ));
        let servicio = ServicioTasa::new(dir.join("cache.json"), ledger).expect("servicio");

        servicio
            .establecer_tasa_manual(dec!(950.25))
            .expect("tasa manual");
        assert!(servicio.en_modo_manual());
        assert_eq!(servicio.info_actual().valor, dec!(950.25));
        assert_eq!(servicio.info_actual().fuente.as_deref(), Some("MANUAL"));

        // El refresco automatico no toca la tasa manual (y no usa red).
        let tras_refresco = servicio.refrescar().await.expect("refrescar");
        assert_eq!(tras_refresco.valor, dec!(950.25));
        assert_eq!(tras_refresco.fuente.as_deref(), Some("MANUAL"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_ida_y_vuelta() {
        let ruta = std::env::temp_dir().join(format!("datio_cache_{}.json", std::process::id()));
        let original = CacheTasa {
            valor: dec!(36.92),
            fecha_unix: 1_700_000_001,
            fuente: "BCV".to_string(),
        };
        guardar_cache(&ruta, &original);
        let leido = cargar_cache(&ruta);
        let _ = std::fs::remove_file(&ruta);
        assert_eq!(leido.map(|c| c.valor), Some(dec!(36.92)));
    }
}
