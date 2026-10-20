use crate::capacidades::{
    CAP_COMISION, CAP_GARANTIA, CAP_PESABLE, CAP_SERIE, CAP_UNITARIA, CAP_VARIANTES, ErrorNegocio,
};
use rust_decimal::Decimal;
use serde::{
    Deserialize, Deserializer, Serialize, Serializer, de::Error as DeError, ser::Error as SerError,
};
use std::str::FromStr;

const MAX_ATRIBUTOS: usize = 8;
const MAX_CLAVE_LEN: usize = 32;
const MAX_VALOR_LEN: usize = 64;
const MAX_SERIE_LEN: usize = 64;
const MAX_VENDEDOR_LEN: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtributoVariante {
    pub clave: [u8; MAX_CLAVE_LEN],
    pub clave_len: u8,
    pub valor: [u8; MAX_VALOR_LEN],
    pub valor_len: u8,
}

impl Default for AtributoVariante {
    fn default() -> Self {
        Self {
            clave: [0u8; MAX_CLAVE_LEN],
            clave_len: 0,
            valor: [0u8; MAX_VALOR_LEN],
            valor_len: 0,
        }
    }
}

impl Serialize for AtributoVariante {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = s.serialize_struct("AtributoVariante", 2)?;
        state.serialize_field(
            "clave",
            std::str::from_utf8(&self.clave[..self.clave_len as usize])
                .map_err(SerError::custom)?,
        )?;
        state.serialize_field(
            "valor",
            std::str::from_utf8(&self.valor[..self.valor_len as usize])
                .map_err(SerError::custom)?,
        )?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for AtributoVariante {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct AttrHelper {
            clave: String,
            valor: String,
        }
        let h = AttrHelper::deserialize(d)?;
        if h.clave.len() > MAX_CLAVE_LEN || h.valor.len() > MAX_VALOR_LEN {
            return Err(DeError::custom("Longitud excedida"));
        }
        let mut clave = [0u8; MAX_CLAVE_LEN];
        let mut valor = [0u8; MAX_VALOR_LEN];
        clave[..h.clave.len()].copy_from_slice(h.clave.as_bytes());
        valor[..h.valor.len()].copy_from_slice(h.valor.as_bytes());
        Ok(Self {
            clave,
            clave_len: h.clave.len() as u8,
            valor,
            valor_len: h.valor.len() as u8,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Variante {
    pub sku: String,
    pub nombre: String,
    pub precio_usd: Decimal,
    pub stock: Decimal,
    pub atributos: [AtributoVariante; MAX_ATRIBUTOS],
    pub num_atributos: u8,
}

impl Serialize for Variante {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = s.serialize_struct("Variante", 6)?;
        state.serialize_field("sku", &self.sku)?;
        state.serialize_field("nombre", &self.nombre)?;
        state.serialize_field("precio_usd", &self.precio_usd.to_string())?;
        state.serialize_field("stock", &self.stock.to_string())?;
        let attrs: Vec<_> = self.atributos[..self.num_atributos as usize]
            .iter()
            .collect();
        state.serialize_field("atributos", &attrs)?;
        state.serialize_field("num_atributos", &self.num_atributos)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for Variante {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct VarianteHelper {
            sku: String,
            nombre: String,
            precio_usd: String,
            stock: String,
            atributos: Vec<AtributoVariante>,
            num_atributos: u8,
        }
        let h = VarianteHelper::deserialize(d)?;
        if h.atributos.len() > MAX_ATRIBUTOS {
            return Err(DeError::custom("Demasiados atributos"));
        }
        let mut attrs = [AtributoVariante::default(); MAX_ATRIBUTOS];
        for (i, a) in h.atributos.into_iter().enumerate() {
            attrs[i] = a;
        }
        Ok(Self {
            sku: h.sku,
            nombre: h.nombre,
            precio_usd: Decimal::from_str(&h.precio_usd).map_err(DeError::custom)?,
            stock: Decimal::from_str(&h.stock).map_err(DeError::custom)?,
            atributos: attrs,
            num_atributos: h.num_atributos,
        })
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Garantia {
    pub sku: String,
    pub serie: [u8; MAX_SERIE_LEN],
    #[allow(dead_code)]
    pub serie_len: u8,
    pub fecha_venta_unix: i64,
    pub duracion_dias: u16,
    pub estado: EstadoGarantia,
}

impl Serialize for Garantia {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = s.serialize_struct("Garantia", 6)?;
        state.serialize_field("sku", &self.sku)?;
        state.serialize_field(
            "serie",
            std::str::from_utf8(&self.serie[..self.serie_len as usize])
                .map_err(SerError::custom)?,
        )?;
        state.serialize_field("serie_len", &self.serie_len)?;
        state.serialize_field("fecha_venta_unix", &self.fecha_venta_unix)?;
        state.serialize_field("duracion_dias", &self.duracion_dias)?;
        state.serialize_field("estado", &self.estado)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for Garantia {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct GarantiaHelper {
            sku: String,
            serie: String,
            fecha_venta_unix: i64,
            duracion_dias: u16,
            estado: EstadoGarantia,
        }
        let h = GarantiaHelper::deserialize(d)?;
        if h.serie.len() > MAX_SERIE_LEN {
            return Err(DeError::custom("Serie demasiado larga"));
        }
        let mut serie = [0u8; MAX_SERIE_LEN];
        serie[..h.serie.len()].copy_from_slice(h.serie.as_bytes());
        Ok(Self {
            sku: h.sku,
            serie,
            serie_len: h.serie.len() as u8,
            fecha_venta_unix: h.fecha_venta_unix,
            duracion_dias: h.duracion_dias,
            estado: h.estado,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EstadoGarantia {
    Vigente,
    Vencida,
    Reclamada,
    Anulada,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ComisionVendedor {
    pub vendedor_id: [u8; MAX_VENDEDOR_LEN],
    pub vendedor_len: u8,
    pub porcentaje: Decimal,
    pub monto_acumulado: Decimal,
}

impl Serialize for ComisionVendedor {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = s.serialize_struct("ComisionVendedor", 4)?;
        state.serialize_field(
            "vendedor_id",
            std::str::from_utf8(&self.vendedor_id[..self.vendedor_len as usize])
                .map_err(SerError::custom)?,
        )?;
        state.serialize_field("vendedor_len", &self.vendedor_len)?;
        state.serialize_field("porcentaje", &self.porcentaje.to_string())?;
        state.serialize_field("monto_acumulado", &self.monto_acumulado.to_string())?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for ComisionVendedor {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct ComisionHelper {
            vendedor_id: String,
            porcentaje: String,
            monto_acumulado: String,
        }
        let h = ComisionHelper::deserialize(d)?;
        if h.vendedor_id.len() > MAX_VENDEDOR_LEN {
            return Err(DeError::custom("Vendedor ID demasiado largo"));
        }
        let mut vendedor_id = [0u8; MAX_VENDEDOR_LEN];
        vendedor_id[..h.vendedor_id.len()].copy_from_slice(h.vendedor_id.as_bytes());
        Ok(Self {
            vendedor_id,
            vendedor_len: h.vendedor_id.len() as u8,
            porcentaje: Decimal::from_str(&h.porcentaje).map_err(DeError::custom)?,
            monto_acumulado: Decimal::from_str(&h.monto_acumulado).map_err(DeError::custom)?,
        })
    }
}

pub const fn capacidades_permitidas() -> u16 {
    CAP_UNITARIA | CAP_PESABLE | CAP_SERIE | CAP_VARIANTES | CAP_GARANTIA | CAP_COMISION
}

pub fn validar_serie(serie: &str) -> Result<(), ErrorNegocio> {
    if serie.is_empty() || serie.len() > MAX_SERIE_LEN {
        return Err(ErrorNegocio::CantidadNoUnitaria(Decimal::ZERO));
    }
    Ok(())
}

pub fn validar_atributo(clave: &str, valor: &str) -> Result<(), ErrorNegocio> {
    if clave.is_empty() || clave.len() > MAX_CLAVE_LEN {
        return Err(ErrorNegocio::CantidadNoUnitaria(Decimal::ZERO));
    }
    if valor.is_empty() || valor.len() > MAX_VALOR_LEN {
        return Err(ErrorNegocio::CantidadNoUnitaria(Decimal::ZERO));
    }
    Ok(())
}

pub fn validar_vendedor(vendedor: &str) -> Result<(), ErrorNegocio> {
    if vendedor.is_empty() || vendedor.len() > MAX_VENDEDOR_LEN {
        return Err(ErrorNegocio::CantidadNoUnitaria(Decimal::ZERO));
    }
    Ok(())
}

pub fn calcular_comision(comision: &ComisionVendedor, monto_venta_usd: Decimal) -> Decimal {
    (monto_venta_usd * comision.porcentaje / Decimal::from(100)).round_dp(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn retail_capacidades_correctas() {
        let caps = capacidades_permitidas();
        assert!(caps & CAP_SERIE != 0);
        assert!(caps & CAP_VARIANTES != 0);
        assert!(caps & CAP_GARANTIA != 0);
        assert!(caps & CAP_COMISION != 0);
    }

    #[test]
    fn serie_validacion_longitud() {
        assert!(validar_serie("ABC123").is_ok());
        assert!(validar_serie("").is_err());
        assert!(validar_serie(&"A".repeat(65)).is_err());
    }

    #[test]
    fn comision_calculo() {
        let comision = ComisionVendedor {
            vendedor_id: [0; MAX_VENDEDOR_LEN],
            vendedor_len: 6,
            porcentaje: dec!(5.0),
            monto_acumulado: dec!(0),
        };
        let venta = dec!(100.0);
        let calc = calcular_comision(&comision, venta);
        assert_eq!(calc, dec!(5.0));
    }
}
