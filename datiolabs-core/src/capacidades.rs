use rust_decimal::Decimal;
use thiserror::Error;

pub const RUBRO_ABASTO: u16 = 1 << 0;
pub const RUBRO_PANADERIA: u16 = 1 << 1;
pub const RUBRO_LICORERIA: u16 = 1 << 2;
pub const RUBRO_RETAIL: u16 = 1 << 3;
pub const RUBRO_UNIVERSAL: u16 = RUBRO_ABASTO | RUBRO_PANADERIA | RUBRO_LICORERIA | RUBRO_RETAIL;

pub const CAP_UNITARIA: u16 = 1 << 0;
pub const CAP_PESABLE: u16 = 1 << 1;
pub const CAP_PERECEDERO: u16 = 1 << 2;
pub const CAP_CUENTA_ABIERTA: u16 = 1 << 3;
pub const CAP_SERIE: u16 = 1 << 4;
pub const CAP_VARIANTES: u16 = 1 << 5;
pub const CAP_GARANTIA: u16 = 1 << 6;
pub const CAP_COMISION: u16 = 1 << 7;
pub const TODAS_LAS_CAPACIDADES: u16 = CAP_UNITARIA
    | CAP_PESABLE
    | CAP_PERECEDERO
    | CAP_CUENTA_ABIERTA
    | CAP_SERIE
    | CAP_VARIANTES
    | CAP_GARANTIA
    | CAP_COMISION;

#[derive(Debug, Error, PartialEq)]
pub enum ErrorNegocio {
    #[error("producto inexistente en el catalogo")]
    ProductoInexistente,
    #[error("stock insuficiente: disponible {disponible}, solicitado {solicitado}")]
    StockInsuficiente {
        disponible: Decimal,
        solicitado: Decimal,
    },
    #[error("cantidad invalida para producto de venta unitaria: {0}")]
    CantidadNoUnitaria(Decimal),
    #[error("lote vencido: caduco en unix {caduco_unix}")]
    LoteVencido { caduco_unix: i64 },
    #[error("sku duplicado en el catalogo: {0}")]
    SkuDuplicado(String),
    #[error("capacidad no activa en la configuracion del negocio: bits solicitados {0:#010b}")]
    CapacidadInactiva(u16),
    #[error("el negocio ya esta inicializado")]
    YaInicializado,
    #[error("el negocio no ha sido inicializado")]
    NoInicializado,
    #[error("cuenta abierta inexistente o ya cerrada: {0}")]
    CuentaInvalida(String),
    #[error("pago insuficiente: requerido {requerido}, recibido {recibido}")]
    PagoInsuficiente {
        requerido: Decimal,
        recibido: Decimal,
    },
    #[error("clave de licencia invalida o no autorizada para este respaldo")]
    LicenciaInvalida,
    #[error("operacion no permitida fuera del turno abierto")]
    FueraDeTurno,
}

/// Deduplicacion por OR de bits: el software unificado activa
/// todas las capacidades del motor de forma nativa e inherente.
pub const fn capacidades_de_rubros(rubros: u16) -> u16 {
    if rubros == 0 {
        TODAS_LAS_CAPACIDADES
    } else {
        let mut caps = CAP_UNITARIA;
        if rubros & RUBRO_ABASTO != 0 {
            caps |= CAP_PESABLE;
        }
        if rubros & RUBRO_PANADERIA != 0 {
            caps |= CAP_PESABLE | CAP_PERECEDERO;
        }
        if rubros & RUBRO_LICORERIA != 0 {
            caps |= CAP_CUENTA_ABIERTA;
        }
        if rubros & RUBRO_RETAIL != 0 {
            caps |= CAP_SERIE | CAP_VARIANTES | CAP_GARANTIA | CAP_COMISION;
        }
        caps
    }
}

pub const fn rubros_activos(rubros: u16) -> bool {
    rubros != 0
        && (rubros & !(RUBRO_ABASTO | RUBRO_PANADERIA | RUBRO_LICORERIA | RUBRO_RETAIL)) == 0
}

/// Validacion pura de una linea de venta contra las capacidades del producto.
/// `cantidad_disponible` proviene del indice SoA del catalogo; cero allocaciones.
pub fn validar_linea(
    capacidades: u16,
    cantidad: Decimal,
    stock_disponible: Decimal,
) -> Result<(), ErrorNegocio> {
    if cantidad <= Decimal::ZERO {
        return Err(ErrorNegocio::CantidadNoUnitaria(cantidad));
    }
    let es_unitaria = capacidades & CAP_PESABLE == 0;
    if es_unitaria && cantidad.fract() != Decimal::ZERO {
        return Err(ErrorNegocio::CantidadNoUnitaria(cantidad));
    }
    if stock_disponible < cantidad {
        return Err(ErrorNegocio::StockInsuficiente {
            disponible: stock_disponible,
            solicitado: cantidad,
        });
    }
    Ok(())
}

/// Verificacion de vigencia para productos perecederos gobernados por lote.
pub fn validar_vigencia(caduce_unix: i64, ahora_unix: i64) -> Result<(), ErrorNegocio> {
    if ahora_unix >= caduce_unix {
        return Err(ErrorNegocio::LoteVencido {
            caduco_unix: caduce_unix,
        });
    }
    Ok(())
}

/// Lista canonica de las 10 claves universales oficiales de DatioLabs.
///
/// Es la unica fuente de verdad: la validacion es por igualdad exacta, sin
/// checksums ni derivaciones que puedan alterarse sin tocar esta lista.
/// La ranura 1 (`0000000000000000`) es una clave de ejemplo (la licencia comercial ya no aplica).
pub const CLAVES_UNIVERSALES: [&str; 10] = [
    "0000000000000000",
    "0000000000000001",
    "0000000000000002",
    "0000000000000003",
    "0000000000000004",
    "0000000000000005",
    "0000000000000006",
    "0000000000000007",
    "0000000000000008",
    "0000000000000009",
];

/// Total de ranuras de licencias universales invariantes admitidas por el sistema.
pub const TOTAL_LICENCIAS_UNIVERSALES: usize = CLAVES_UNIVERSALES.len();

/// Normaliza una clave quitando todo lo que no sea digito (guiones, espacios).
pub fn normalizar_licencia(clave: &str) -> String {
    clave.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Genera la clave universal de una ranura especifica (0..10) desde la lista canonica.
pub fn generar_licencia_slot(slot: usize) -> String {
    CLAVES_UNIVERSALES[slot % TOTAL_LICENCIAS_UNIVERSALES].to_string()
}

/// Genera el conjunto completo de las 10 licencias universales invariantes.
pub fn generar_10_licencias_universales() -> [String; TOTAL_LICENCIAS_UNIVERSALES] {
    core::array::from_fn(generar_licencia_slot)
}

/// Genera una licencia universal eligiendo una de las 10 ranuras invariantes oficiales.
pub fn generar_licencia_universal() -> String {
    use rand::RngExt;
    let mut rng = rand::rng();
    let slot = rng.random_range(0..TOTAL_LICENCIAS_UNIVERSALES);
    generar_licencia_slot(slot)
}

/// Valida una clave universal por igualdad exacta contra la lista canonica.
pub fn validar_licencia_universal(clave: &str) -> bool {
    let limpio = normalizar_licencia(clave);
    CLAVES_UNIVERSALES.iter().any(|k| *k == limpio)
}

/// Alias historico del validador estricto por lista canonica.
pub fn es_licencia_universal_autorizada(clave: &str) -> bool {
    validar_licencia_universal(clave)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn union_deduplica_capacidades_compartidas() {
        assert_eq!(
            capacidades_de_rubros(RUBRO_ABASTO),
            CAP_UNITARIA | CAP_PESABLE
        );
        let ambos = capacidades_de_rubros(RUBRO_ABASTO | RUBRO_PANADERIA);
        assert_eq!(ambos.count_ones(), 3);
        assert_eq!(ambos & CAP_PESABLE, CAP_PESABLE);
        let todos = capacidades_de_rubros(RUBRO_ABASTO | RUBRO_PANADERIA | RUBRO_LICORERIA);
        assert_eq!(todos.count_ones(), 4);
    }

    #[test]
    fn rubros_validos_solo_con_bits_conocidos() {
        assert!(rubros_activos(RUBRO_LICORERIA));
        assert!(rubros_activos(RUBRO_ABASTO | RUBRO_PANADERIA));
        assert!(!rubros_activos(0));
        assert!(!rubros_activos(0b1000_0000));
    }

    #[test]
    fn linea_unitaria_rechaza_fracciones_y_falta_stock() {
        let caps = CAP_UNITARIA;
        assert!(validar_linea(caps, dec!(3), dec!(10)).is_ok());
        assert_eq!(
            validar_linea(caps, dec!(1.5), dec!(10)),
            Err(ErrorNegocio::CantidadNoUnitaria(dec!(1.5)))
        );
        assert_eq!(
            validar_linea(caps, dec!(11), dec!(10)),
            Err(ErrorNegocio::StockInsuficiente {
                disponible: dec!(10),
                solicitado: dec!(11)
            })
        );
    }

    #[test]
    fn pesable_admite_fracciones() {
        assert!(validar_linea(CAP_PESABLE, dec!(0.750), dec!(5)).is_ok());
        assert!(validar_linea(CAP_UNITARIA, dec!(0), dec!(5)).is_err());
    }

    #[test]
    fn vigencia_bloquea_en_y_despues_del_limite() {
        assert!(validar_vigencia(2000, 1999).is_ok());
        assert_eq!(
            validar_vigencia(2000, 2000),
            Err(ErrorNegocio::LoteVencido { caduco_unix: 2000 })
        );
    }

    #[test]
    fn licencia_canonica_solo_acepta_las_diez_claves() {
        // La licencia perpetua vendida al cliente es la ranura 1.
        assert_eq!(generar_licencia_slot(0), "0000000000000000");
        assert!(validar_licencia_universal("0000000000000000"));
        assert!(validar_licencia_universal("0000-0000-0000-0000"));
        assert!(es_licencia_universal_autorizada("0000 0000 0000 0000"));

        // Las 10 canonicas validan y son unicas.
        let diez = generar_10_licencias_universales();
        let mut set = std::collections::HashSet::new();
        for lic in &diez {
            assert_eq!(lic.len(), 16);
            assert!(validar_licencia_universal(lic));
            assert!(es_licencia_universal_autorizada(lic));
            set.insert(lic.clone());
        }
        assert_eq!(set.len(), 10, "las 10 licencias deben ser unicas");

        // Invalidas: formato y claves con checksum valido pero fuera de la lista.
        assert!(!validar_licencia_universal("123456789012345"));
        assert!(!validar_licencia_universal("12345678901234567"));
        assert!(!validar_licencia_universal(""));
        assert!(!validar_licencia_universal("abcdefghijklmnop"));
        // Clave del txt corrupto del 15-sep (checksum peso valido, no canonica).
        assert!(!validar_licencia_universal("1662373670170269"));

        for _ in 0..50 {
            let lic = generar_licencia_universal();
            assert!(validar_licencia_universal(&lic));
        }
    }
}
