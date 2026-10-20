#!/usr/bin/env python3
"""Audita el contrato Rust <-> TypeScript de los comandos Tauri.

Vive en el repo del core porque las dos fuentes de verdad (``datiolabs-ui/src/lib.rs``
y ``datiolabs-core/src/capacidades.rs``) estan aqui. El frontend se clona en ``ui/``
(repo datiolabs-frontend) antes de correr esto; si falta, falla en vez de saltarse.

Port del antiguo ``ui/tests/contract_tauri.test.ts``: mismas comprobaciones
principales, sin necesitar Node. Cubre comandos, argumentos y registro; no
valida campos de DTO.

Limitaciones del parser: ignora llamadas con spread (``...``), genericos
anidados y objetos anidados con comas; una llamada cuyo objeto viene en una
variable no se puede verificar. Es un gate para el codigo actual, no un parser
de TypeScript.

Uso:
    git clone https://github.com/miguelcorderopamphile/datiolabs-frontend.git ui
    python3 scripts/auditar_contrato_tauri.py [--ui <ruta>]

Sale con codigo 1 si hay desajustes.
"""

import argparse
import pathlib
import re
import sys

RAIZ = pathlib.Path(__file__).resolve().parent.parent
RUTA_UI = RAIZ / "ui"
RUTA_LIB_RS = RAIZ / "datiolabs-ui" / "src" / "lib.rs"
RUTA_CAPACIDADES = RAIZ / "datiolabs-core" / "src" / "capacidades.rs"

INYECTADOS = re.compile(r"tauri::State|AppHandle|tauri::Window|tauri::WebviewWindow")

# Desajustes conocidos y pendientes de decision de producto. Vacio: todo alineado.
PENDIENTES: dict[str, str] = {}

GUARDAS_ESPERADAS = {
    "abonar_cuenta",
    "abrir_cuenta",
    "agregar_consumo",
    "cerrar_cuenta",
    "editar_abono_cuenta",
    "eliminar_consumo",
    "eliminar_venta",
    "registrar_venta",
}


def bloque_balanceado(texto: str, desde: int, apertura: str, cierre: str) -> str:
    """Contenido entre ``apertura`` y su ``cierre`` balanceado, saltando strings."""
    nivel = 0
    i = desde
    while i < len(texto):
        c = texto[i]
        if c in "\"'`":
            comilla = c
            i += 1
            while i < len(texto) and texto[i] != comilla:
                if texto[i] == "\\":
                    i += 1
                i += 1
            i += 1
            continue
        if c == apertura:
            nivel += 1
        elif c == cierre:
            nivel -= 1
            if nivel == 0:
                return texto[desde + 1 : i]
        i += 1
    return texto[desde + 1 :]


def separar_parametros(bloque: str) -> list[str]:
    """Parte por comas de primer nivel (ignora las de dentro de ``<...>``)."""
    partes: list[str] = []
    nivel = 0
    actual = ""
    for c in bloque:
        if c == "<":
            nivel += 1
        if c == ">":
            nivel -= 1
        if c == "," and nivel == 0:
            partes.append(actual)
            actual = ""
            continue
        actual += c
    if actual.strip():
        partes.append(actual)
    return partes


def a_camel(s: str) -> str:
    return re.sub(r"_([a-z0-9])", lambda m: m.group(1).upper(), s)


def leer_comandos_rust(codigo: str) -> dict[str, dict]:
    comandos: dict[str, dict] = {}
    desde = 0
    while True:
        marca = codigo.find("#[tauri::command]", desde)
        if marca == -1:
            break
        desde = marca + 1

        fn = codigo.find("fn ", marca)
        if fn == -1:
            continue
        m = re.match(r"fn\s+([A-Za-z0-9_]+)\s*\(", codigo[fn:])
        if not m:
            continue
        nombre = m.group(1)
        par_abre = codigo.find("(", fn + m.end() - 1)
        bloque = bloque_balanceado(codigo, par_abre, "(", ")")

        admite: set[str] = set()
        params: list[dict] = []
        for parte in separar_parametros(bloque):
            param = parte.strip()
            if not param or param.startswith("//"):
                continue
            corte = param.find(":")
            if corte == -1:
                continue
            crudo = param[:corte]
            tipo = param[corte + 1 :]
            if not crudo.strip() or not tipo.strip():
                continue
            if INYECTADOS.search(tipo):
                continue
            limpio = re.sub(r"^(mut|ref)\s+", "", crudo.strip())
            if not limpio:
                continue
            camel = a_camel(limpio)
            admite.add(limpio)
            admite.add(camel)
            if limpio.startswith("_"):
                sin_guion = limpio.lstrip("_")
                admite.add(sin_guion)
                admite.add(a_camel(sin_guion))
            params.append({"snake": limpio, "camel": camel, "opcional": "Option<" in tipo})

        comandos[nombre] = {"nombre": nombre, "admite": admite, "params": params}
    return comandos


def cuerpos_de_comandos(codigo: str) -> dict[str, str]:
    marcas = [m.start() for m in re.finditer(re.escape("#[tauri::command]"), codigo)]
    salida: dict[str, str] = {}
    for k, desde in enumerate(marcas):
        hasta = marcas[k + 1] if k + 1 < len(marcas) else len(codigo)
        bloque = codigo[desde:hasta]
        m = re.search(r"fn\s+([a-z0-9_]+)\s*\(", bloque)
        if m:
            salida[m.group(1)] = bloque
    return salida


def leer_registrados(codigo: str) -> set[str]:
    marca = codigo.find("generate_handler!")
    if marca == -1:
        return set()
    corchete = codigo.find("[", marca)
    bloque = bloque_balanceado(codigo, corchete, "[", "]")
    return {
        s.strip()
        for s in bloque.split(",")
        if re.fullmatch(r"[A-Za-z0-9_]+", s.strip())
    }


def archivos_ts(directorio: pathlib.Path) -> list[pathlib.Path]:
    return sorted(directorio.rglob("*.ts"))


def leer_llamadas(codigo: str, archivo: str) -> list[dict]:
    llamadas: list[dict] = []
    patron = re.compile(r"\binvocar\s*(?:<[^<>]*>)?\s*\(")
    for m in patron.finditer(codigo):
        par_abre = codigo.find("(", m.start() + len(m.group(0)) - 1)
        bloque = bloque_balanceado(codigo, par_abre, "(", ")")
        arg = re.match(r"\s*['\"]([a-z0-9_]+)['\"]", bloque)
        if not arg:
            continue
        comando = arg.group(1)
        resto = bloque[arg.end() :]
        llave = resto.find("{")
        linea = codigo[: m.start()].count("\n") + 1
        if llave == -1:
            llamadas.append({"comando": comando, "claves": [], "archivo": archivo, "linea": linea})
            continue
        cuerpo = bloque_balanceado(resto, llave, "{", "}")
        if "..." in cuerpo:
            continue
        claves = []
        for s in cuerpo.split(","):
            s = s.strip()
            if re.match(r"^[A-Za-z0-9_]+(?:\s*:|\s*$)", s):
                claves.append(s.split(":")[0].strip())
        llamadas.append(
            {"comando": comando, "claves": [c for c in claves if c], "archivo": archivo, "linea": linea}
        )
    return llamadas


def claves_universales(codigo: str) -> list[str]:
    marca = codigo.find("CLAVES_UNIVERSALES")
    if marca == -1:
        return []
    igual = codigo.find("=", marca)
    abre = codigo.find("[", igual)
    cierra = codigo.find("]", abre)
    if abre == -1 or cierra == -1:
        return []
    return re.findall(r"['\"](\d{16})['\"]", codigo[abre:cierra])


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--ui", default=str(RUTA_UI), help="raiz del frontend (default: ui/)")
    args = ap.parse_args()
    ruta_ui = pathlib.Path(args.ui).resolve()
    ruta_api_ts = ruta_ui / "src" / "negocio" / "api.ts"
    ruta_src = ruta_ui / "src"
    if not ruta_api_ts.is_file():
        print(f"FALLA: falta {ruta_api_ts}. Clona el frontend o pasa --ui <ruta>")
        return 1

    lib_rs = RUTA_LIB_RS.read_text()
    comandos = leer_comandos_rust(lib_rs)
    registrados = leer_registrados(lib_rs)
    api_ts = ruta_api_ts.read_text()
    llamadas = [l for ruta in archivos_ts(ruta_src) for l in leer_llamadas(ruta.read_text(), str(ruta.relative_to(ruta_ui)))]

    fallos: list[str] = []

    if not (len(comandos) > 50 and len(registrados) > 50 and len(llamadas) > 50):
        fallos.append(
            f"parser vacio: comandos={len(comandos)} registrados={len(registrados)} llamadas={len(llamadas)}"
        )

    for l in llamadas:
        if l["comando"] not in comandos:
            fallos.append(f"{l['archivo']}:{l['linea']} -> {l['comando']}() no existe en lib.rs")

    for nombre in sorted(set(comandos) - registrados):
        fallos.append(f"{nombre}() no esta en generate_handler!")

    for l in llamadas:
        cmd = comandos.get(l["comando"])
        if not cmd:
            continue
        for clave in l["claves"]:
            if clave in cmd["admite"]:
                continue
            firma = f"{l['comando']}.{clave}"
            if firma not in PENDIENTES:
                fallos.append(
                    f"{l['archivo']}:{l['linea']} {l['comando']}({{ {clave}: ... }}) <- no existe en lib.rs"
                )

    cuerpos = cuerpos_de_comandos(lib_rs)
    con_guarda = {n for n, c in cuerpos.items() if "jornada_requerida(&estado)?" in c}
    if con_guarda != GUARDAS_ESPERADAS:
        fallos.append(
            f"guardas de turno: sobran={sorted(con_guarda - GUARDAS_ESPERADAS)} faltan={sorted(GUARDAS_ESPERADAS - con_guarda)}"
        )

    casos = set(re.findall(r"case\s+'([a-z0-9_]+)'", api_ts))
    for caso in sorted(casos - set(comandos)):
        fallos.append(f"mock case '{caso}' sin comando Rust")
    if "httpRoutes" in api_ts:
        fallos.append("api.ts aun contiene httpRoutes (el panel HTTP fue purgado)")

    claves_rust = claves_universales(RUTA_CAPACIDADES.read_text())
    claves_ts = claves_universales(api_ts)
    if len(claves_rust) != 10 or len(claves_ts) != 10 or claves_ts != claves_rust:
        fallos.append(f"licencias: rust={len(claves_rust)} ts={len(claves_ts)} iguales={claves_ts == claves_rust}")
    if "0000000000000000" not in claves_rust:
        fallos.append("licencias: falta la perpetua 0000000000000000")

    for l in llamadas:
        cmd = comandos.get(l["comando"])
        if not cmd:
            continue
        enviadas = set(l["claves"])
        for p in cmd["params"]:
            if p["opcional"]:
                continue
            if p["camel"] not in enviadas and p["snake"] not in enviadas:
                fallos.append(f"{l['archivo']}:{l['linea']} {l['comando']}() no envia {p['camel']}")

    if fallos:
        print(f"FALLA: {len(fallos)} desajuste(s)")
        for f in fallos:
            print("  -", f)
        return 1

    print(
        f"OK: {len(comandos)} comandos, {len(registrados)} registrados, "
        f"{len(llamadas)} llamadas, guardas y licencias alineadas"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
