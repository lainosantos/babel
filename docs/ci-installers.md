# Instaladores no GitHub Actions

O workflow **Babel CI and installers** (`.github/workflows/ci.yml`) roda em push,
pull request e acionamento manual. Primeiro exige testes Rust, Clippy, formatação,
testes da interface e smoke da CLI nos três sistemas. Em seguida chama o workflow
reutilizável **Build installers and native drivers**. Esse segundo workflow também
pode ser acionado manualmente para reconstruir pacotes; ele executa seus próprios
testes dos núcleos nativos e dos empacotadores.

Todos os jobs usam runners hospedados pelo GitHub. Não é necessário manter um
runner `self-hosted`, instalar o WDK manualmente no runner ou fornecer chaves de
API dos provedores de IA. O repositório precisa conter todos os fontes, manifests,
lockfiles, scripts e workflows; pastas de build e configurações pessoais não devem
ser versionadas.

## Onde baixar

Abra **Actions → Babel CI and installers → execução → Artifacts**. Depois de todos
os testes e builds correspondentes passarem, os seguintes artefatos ficam
armazenados por 30 dias:

| Artifact | Conteúdo | Runner |
|---|---|---|
| `babel-installers-linux-amd64` | `.deb`, `.rpm`, `.tar.gz`, manifestos e hashes | Ubuntu 22.04 x64 |
| `babel-installers-macos-universal-development` | `.pkg` com Babel.app + driver HAL, manifesto e hashes | macOS 15 / SDK Apple |
| `babel-installers-windows-x64-development` | instalador `.exe`, `.zip` completo, manifesto e hashes | Windows Server 2022 / VS2022 |
| `babel-installers-windows-ARM64-development` | os mesmos formatos, payload ARM64 | Windows Server 2022 com compilação cruzada |

O nome dos arquivos inclui a versão de `Cargo.toml`. O `.pkg` macOS contém código
Intel e Apple Silicon. O Windows usa instaladores separados e verifica a
arquitetura nativa do sistema; o driver x64 não é oferecido por emulação em ARM64.
No Linux, a dependência mínima de glibc é extraída dos binários realmente gerados.
O `.deb` é destinado a Debian/Ubuntu compatíveis; o `.rpm` atende distribuições
RPM compatíveis, como Fedora. Outras distribuições podem usar o arquivo portátil
e instalar as dependências indicadas no guia Linux.

Uma falha no build, nos testes do pacote ou na ausência de um arquivo obrigatório
impede o upload daquele instalador. Logs de falhas WDK são preservados separadamente.
Os artefatos são downloads da execução; o workflow não cria automaticamente uma
GitHub Release nem publica arquivos em outro serviço.

## O que cada build verifica

### Linux

Compila os executáveis de release, executa `--help`, `--version` e `init` com
configuração temporária, monta e inspeciona o `.deb`, `.rpm` e `.tar.gz`. Confere layout,
permissões e integridade dos binários. Inclui launcher, entrada de menu, ícone,
documentação e licenças. O pacote não inicia Babel, ativa autostart ou altera o
servidor de áudio durante a instalação. Os pacotes incluem `org.babel.audio.service`
para o gerenciador systemd do usuário; a opção de início automático do Babel
habilita/desabilita o serviço quando disponível e conserva XDG como alternativa.
Não é um daemon de áudio executado como root. PulseAudio/PipeWire-pulse continua sendo
o backend do Linux; `pulseaudio-utils` disponibiliza `pactl`.

### macOS

Compila o app e o HAL para `aarch64-apple-darwin` e `x86_64-apple-darwin`, reúne as
slices com `lipo` e verifica as arquiteturas. O teste do HAL carrega o plug-in
somente em um processo de teste, fora do CoreAudio do runner. O empacotador confere
o bundle, seus binários e a expansão do `.pkg` antes de disponibilizá-lo.
A entrada do aplicativo é o binário Rust `babel-tray`, com a declaração de acesso
a microfone no `Info.plist`. O instalador inclui tanto o app em `/Applications`
quanto o componente original do driver em `/Library/Audio/Plug-Ins/HAL`.

### Windows

O setup restaura **SDK e WDK oficiais via NuGet**, em versões e hashes fixados em
`native/windows/wdk-packages.json`. Usa as ferramentas C++/WDK do Visual Studio
2022 presentes na imagem hospedada e verifica os componentes antes do build.
Headers, bibliotecas e ferramentas não dependem de downloads de uma branch mutável.
O WDK gera e valida o pacote `INF/SYS/CAT`; o helper de instalação é compilado em
Rust. O aplicativo usa o runtime C estático, evitando exigir um redistribuível
Visual C++ separado para iniciar o app.

O Inno Setup gera o instalador gráfico com app, pacote completo do driver,
documentação, licenças e atalho. O empacotador rejeita mistura de arquiteturas,
INF ainda não processado e ausência de catálogo/helper. Verifica todos os arquivos
do ZIP contra o manifesto. O job x64 adicionalmente instala o **aplicativo** em
uma pasta temporária do runner, compara seus arquivos com o manifesto, executa
apenas `babel --version`, confirma que nenhum driver Babel foi criado e testa a
desinstalação. Não abre áudio nem instala o driver no runner. ARM64 passa pelo
build e inspeção de payload; não é executado no host x64.

## Assinatura e alcance da validação

Os pacotes macOS/Windows são explicitamente identificados como **development**.
Não contêm credenciais de assinatura e não se apresentam como uma distribuição
assinada/notarizada:

- No macOS, a assinatura ad-hoc permite verificar a estrutura no CI. Distribuição
  pública exige as identidades Developer ID apropriadas e notarização.
- No Windows, gerar `.cat` não equivale a assiná-lo. O `.exe` instala o aplicativo
  e disponibiliza os arquivos do driver, mas o driver sem assinatura não é
  ativado automaticamente. A ativação normal requer o pacote assinado segundo
  a política Microsoft e execução explícita de `install.ps1` como administrador.
  O instalador não muda Secure Boot, não habilita test-signing e não instala
  certificados. Para remover o app após ativar o driver, execute primeiro
  `uninstall.ps1`; o desinstalador preserva o helper enquanto existir um devnode.

Um job verde comprova as verificações descritas, não a estabilidade de áudio em
hardware. Permissões de microfone, qualidade, latência, suspensão/retomada,
Driver Verifier e uso em chamadas continuam exigindo os cenários de
[teste nativo](testing.md). A primeira execução no GitHub precisa ser consultada;
validação local de YAML/scripts não é um run hospedado bem-sucedido.

## Configuração do aplicativo instalado

A bandeja usa uma configuração gravável por usuário, sem depender do diretório
corrente ou da pasta protegida do aplicativo:

- Linux: `$XDG_CONFIG_HOME/babel/babel.toml`, se o prefixo for absoluto; caso
  contrário, `~/.config/babel/babel.toml`.
- macOS: `~/Library/Application Support/Babel/babel.toml`.
- Windows: `%APPDATA%\Babel\babel.toml`, com fallback para a pasta Roaming do usuário.

`--config` continua permitindo escolher outro arquivo. A CLI `babel` mantém o
comportamento explícito de desenvolvimento com `babel.toml` no diretório corrente.
Autostart é uma escolha do usuário nas configurações. O painel continua usando
porta dinâmica. Nenhum pacote inclui API keys, modelos, gravações, transcrições
ou configurações da máquina de desenvolvimento.

## Reproduzir localmente

As instruções e argumentos estão em:

- [Linux](../packaging/linux/README.md).
- [macOS](../packaging/macos/README.md).
- [Windows](../packaging/windows/README.md).
- [Drivers nativos](native-drivers.md), incluindo compilação e assinatura.

Referências primárias: [runners hospedados](https://docs.github.com/en/actions/reference/runners/github-hosted-runners),
[imagem Windows 2022](https://github.com/actions/runner-images/blob/main/images/windows/Windows2022-Readme.md),
[WDK em CI](https://techcommunity.microsoft.com/blog/windowsdriverdev/building-windows-driver-projects-with-ci-and-cd/4379200)
e [arquiteturas de instaladores de drivers](https://jrsoftware.org/ishelp/topic_setup_architecturesallowed.htm).
