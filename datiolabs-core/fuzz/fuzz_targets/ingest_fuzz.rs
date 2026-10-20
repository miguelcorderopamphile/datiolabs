#![no_main]

use libfuzzer_sys::fuzz_target;
use datiolabs_core::ingest::{SensorFeatures, ingest_column};

fuzz_target!(|data: &[u8]| {
    // Escudo contra la entropía: verificamos que ninguna combinación
    // de bytes crudos, malformados, vacíos o gigantes colapse el extractor.
    let _ = SensorFeatures::extract(data);
    
    // Verificamos el pipeline completo de proyección (hasta LSH)
    let _ = ingest_column(data);
});
