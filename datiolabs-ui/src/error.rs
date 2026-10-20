use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct UIError {
    pub message: String,
    pub diagnosis: String,
}

impl UIError {
    pub fn new(message: impl Into<String>, diagnosis: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            diagnosis: diagnosis.into(),
        }
    }
}

// Para retrocompatibilidad con las partes del sistema que aún escupen Strings.
// Inferimos el tipo de error basándonos en palabras clave del String.
impl From<String> for UIError {
    fn from(err: String) -> Self {
        Self {
            message: err.clone(),
            diagnosis: "Error genérico del sistema".to_string(),
        }
    }
}

impl From<reqwest::Error> for UIError {
    fn from(err: reqwest::Error) -> Self {
        use std::error::Error;
        // Incluye la cadena de causas (TLS, DNS, timeout) para diagnostico real.
        let mut detalle = err.to_string();
        let mut fuente = err.source();
        while let Some(causa) = fuente {
            detalle.push_str(" | ");
            detalle.push_str(&causa.to_string());
            fuente = causa.source();
        }
        Self {
            message: detalle,
            diagnosis: "Fallo de red o transporte HTTP durante la operacion solicitada".to_string(),
        }
    }
}

impl From<datiolabs_core::db::DbError> for UIError {
    fn from(err: datiolabs_core::db::DbError) -> Self {
        Self {
            message: err.to_string(),
            diagnosis: "Fallo del almacenamiento local embebido (Sled)".to_string(),
        }
    }
}

impl From<datiolabs_core::capacidades::ErrorNegocio> for UIError {
    fn from(err: datiolabs_core::capacidades::ErrorNegocio) -> Self {
        Self {
            message: err.to_string(),
            diagnosis: "Regla de negocio violada".to_string(),
        }
    }
}

impl std::fmt::Display for UIError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.diagnosis)
    }
}

impl std::error::Error for UIError {}
