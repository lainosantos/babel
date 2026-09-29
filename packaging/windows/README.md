# Instalador Windows

O GitHub Actions executa este empacotador em `windows-2022`, com Python 3.12,
Inno Setup 6.3+ (incluído na imagem) e os binários já compilados. O setup do WDK
é descrito em [native/windows](../../native/windows/README.md).

```powershell
python packaging/windows/build.py --architecture x64 `
  --bin-dir target/x86_64-pc-windows-msvc/release `
  --driver-dir native/windows/dist/x64 --output artifacts/installers
```

Para ARM64 use `--architecture ARM64`, target `aarch64-pc-windows-msvc` e driver
`dist/ARM64`. `--iscc` permite indicar o caminho do compilador Inno Setup.
`--version` aceita três componentes numéricos; por padrão vem de `Cargo.toml`.

São gerados `Babel-<versão>-windows-<arquitetura>-development.exe`, `.zip`,
`-manifest.json` e `-SHA256SUMS.txt`. A pasta de saída não pode conter um pacote
com os mesmos nomes. O ZIP preserva o layout esperado pelo app, incluindo
`drivers/windows/{BabelAudio.inf,BabelAudio.sys,BabelAudio.cat,...}`. Não misture
componentes de builds distintos. A checagem de PE verifica a arquitetura dos dois
executáveis do app, do helper e do driver antes de empacotar.

Estes são artefatos de desenvolvimento sem assinatura de distribuição. A presença
do CAT não significa que ele esteja assinado. O instalador deixa o driver disponível
para a etapa administrativa explícita; não tenta carregar o kernel driver sem
assinatura. Consulte o texto mostrado pelo próprio instalador em
[INSTALLATION.txt](INSTALLATION.txt).

O aplicativo usa `%APPDATA%\Babel\babel.toml`, não grava em Program Files e não
inicia durante o setup. O app não habilita autostart por padrão. A desinstalação
consulta `check-absent` do helper (somente leitura) antes de prosseguir: se o driver
foi instalado, é preciso removê-lo com `uninstall.ps1` primeiro. Assim não remove
um driver antes da confirmação do desinstalador nem apaga o helper necessário.

Testes portáveis de montagem/validação:

```sh
python3 -m unittest discover -s packaging/windows -p 'test_*.py' -v
```

O CI x64 usa `.github/scripts/test-windows-package.ps1` para instalar/remover o
**aplicativo** em um runner descartável, comparar hashes do payload e executar
`--version`. O script recusa execução fora do ambiente GitHub Actions e não
instala o driver. ARM64 é inspecionado/compilado sem execução no host x64.
