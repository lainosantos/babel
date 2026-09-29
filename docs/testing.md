# Verificação e limites dos testes

## CI nativo e smoke sem dispositivos

A matriz em `.github/workflows/ci.yml` executa formatação, Clippy, testes Rust,
testes do bridge Needle e build em Linux, macOS e Windows. Falha em um
SO não cancela os outros. Ela também executa um smoke do binário nativo, sem
iniciar áudio, servidor, bandeja, instalação de drivers ou provider de IA:

```sh
python3 scripts/test_cli_smoke.py --babel target/release/babel
```

No Windows, use `python scripts/test_cli_smoke.py`; o caminho padrão seleciona
`target/release/babel.exe`. O script exige Python 3.11+ e testa `--help`,
`--version` e `init` com configuração temporária, caminho com espaços/Unicode,
padrões específicos do SO e recusa de sobrescrita. `--output-dir` escolhe a
pasta do relatório JSON. Não usa `babel.toml` nem credenciais do usuário.

O CI disponibiliza `test-evidence-<SO>` com toolchain, log dos testes Rust e
relatório do smoke por 14 dias. Após os testes, gera instaladores de release
completos por plataforma, incluindo os drivers macOS/Windows, e preserva esses
artefatos por 30 dias. Consulte [instaladores no CI](ci-installers.md) para nomes,
assinatura e verificações de cada pacote. A existência do workflow não significa
que ele já executou: consulte o resultado do run e os relatórios de cada SO.

Na validação local dos instaladores, **303 testes Rust e 72 testes da interface
passaram**; sete testes que dependem do ambiente ficaram ignorados. Os testes
dos empacotadores também passaram: oito Linux, catorze macOS e três Windows.
O teste Linux gera e inspeciona DEB, RPM e tar.gz reais com executáveis de teste,
sem instalá-los. Os contratos de macOS/Windows são testados com fixtures;
compilação com os SDKs nativos e execução dos instaladores exigem os respectivos
runners. As regressões da interface incluem integração systemd/XDG, identificação
da entrada de login e preservação de rascunhos ao trocar idioma. Não foram
executados drivers, captura ou reprodução nativos macOS/Windows nesta validação.

## Diagnóstico dos serviços de comandos de voz

Após separar falhas de configuração e falhas de comandos, **225 testes Rust e
52 testes DOM passaram**, com Clippy sem avisos. As regressões cobrem Whisper
inválido antes da palavra de ativação, erro de ASR após um comando anterior,
falha depois de uma nova ativação e cancelamento quando o microfone fica inativo.
Um endpoint que imita somente `/health` não recebe WAV de microfone.

Na interface, o diagnóstico de serviço permanece na aba Comandos e desaparece
na recuperação. Navegação para Configurações, mudança de idioma e IDs de
ativações anteriores não geram avisos flutuantes de comando. Os testes também
verificam preservação de rascunhos e acesso aos ajustes locais sem requisições de
inferência ou chamadas de ferramentas.

## Troca da saída virtual para a física

Verificado em 29/09/2026: **219 testes Rust e 50 testes DOM passaram**. Os seis
testes históricos dependentes do ambiente permaneceram ignorados. O cenário
privado descrito abaixo passou com o executável real, incluindo pausa/retomada
na mesma sessão de gravação.

O teste abaixo usa PipeWire, pipewire-pulse, WirePlumber com perfil `policy` e
D-Bus **privados**, em uma pasta temporária. Não abre dispositivos ALSA/Bluetooth,
não lê microfones reais e não muda os padrões da sessão de áudio do desktop.
Exige esses quatro executáveis e `pactl`/`pacat`/`parec` no PATH ou em
`.tools/pulse/usr/bin`; `--pulse-tools` permite indicar outro diretório.

```sh
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo build --bin babel --locked
python3 scripts/test_audio_routing.py --babel target/debug/babel
```

O cenário verifica a proteção contra mover os próprios streams do Babel,
ausência de captura/reprodução quando nenhum aplicativo usa os virtuais,
ativação independente de microfone/saída, troca do aplicativo para o dispositivo
físico simulado e retorno ao Babel. Repete a sequência numa sessão com gravação:
o ID e o único WAV são preservados, enquanto os subprocessos de áudio são novos.
Sem `--babel`, o script verifica apenas o pinning dos clientes PulseAudio.

As regressões Rust em `engine::activity` verificam o descarte de respostas
atrasadas, a atualização do físico durante uma pausa, transições rápidas,
erro do monitor, fechamento ordenado antes de reabrir e continuidade do writer.
As regressões DOM verificam o estado de espera, medidores zerados, direções
independentes e preservação da sessão em inglês e português.

## Atualização de idiomas da interface

Após adicionar inglês/português: **159 testes Rust** e **22 testes DOM** passaram;
`cargo fmt --check`, Clippy com `-D warnings`, build release e check de todos os
targets para `x86_64-pc-windows-gnu` também passaram. Os seis testes dependentes
do ambiente continuam ignorados na suíte normal.

No painel real, foram verificados o padrão do sistema (`en-US` neste ambiente),
a mudança para português, o retorno a `system` e a persistência após recarregar.
A interface foi conferida na largura normal e em 360 px, sem transbordamento
horizontal. Durante as trocas de idioma, os contadores de captura continuaram
crescendo nas duas rotas, com zero frames descartados e zero faltas de reprodução.
Não foram iniciadas traduções, gravações ou chamadas de IA nesse teste de idioma.
A atualização em sessão ativa é coberta por testes do controlador e do DOM;
não foi usada uma sessão de nuvem autenticada para isso.

Verificação executada em **29/09/2026**, em Linux com PipeWire 1.6.2 e
`pipewire-pulse`. Os testes com áudio usaram somente tons sintéticos e
dispositivos virtuais, incluindo sinks temporários isolados. Nenhum deles capturou um microfone físico nem
reproduziu áudio em alto-falantes físicos.

## Suíte automática

```sh
cargo test --all-targets
```

Os testes automáticos passaram, sem falhas. Seis verificações dependentes do
ambiente ficam ignoradas por padrão: áudio de baixo nível, `live_engine`,
dois cenários de `recorded_session`, o launcher XDG/GIO e a espera da bandeja Linux. A suíte normal funciona sem credenciais de IA,
modelos locais carregados ou dispositivos virtuais instalados.

Ela verifica, entre outros comportamentos:

- Idioma da interface: preferência persistente, padrão do sistema, variantes
  regionais e inglês como fallback; catálogos de painel/bandeja e troca durante
  sessão sem cancelar o worker, alterar dispositivos ou sobrescrever rascunhos.

- Limites de filas, interrupção mesmo com fila cheia e propagação de falhas.
- Resampling com rejeição de aliasing e continuidade entre blocos.
- WebSockets reais contra servidores locais de teste: confirmação de setup antes
  de transmitir áudio, streaming de PCM, transcrições, reconexão e cancelamento.
- HTTP local contra mocks de whisper.cpp, Ollama e Piper; multipart WAV,
  segmentação VAD, alinhamento de texto e limites de resposta.
- Uso de voz externa com provider local sem chamar Piper nem gerar WAV descartado.
- Síntese e gestão de vozes contra mocks HTTP; autenticação, limites de upload,
  respostas malformadas e erros sem exposição de chaves ou conteúdo privado.
- Persistência de transcrições originais, metadados legítimos e permissões de arquivo.
- WAV único com mistura simultânea, headroom, gaps, relógio monotônico,
  cabeçalhos/checkpoints, drenagem e recusa de sobrescrita.
- Troca de dispositivos com canais estáveis, cancelamento e recuperação de erros.
- Encaminhamento original com PCM preservado, filas limitadas, descarte de áudio
  obsoleto e cancelamento mesmo quando a saída está saturada.
- Autorização do painel, rejeição de origem indevida e credenciais temporárias sem
  persistência na configuração.

Mocks validam o protocolo implementado; não medem a qualidade de tradução, a
latência de um serviço remoto ou a disponibilidade de um modelo na sua conta.

## Integração com o servidor de áudio Linux

É necessário ter `pactl`, `parec` e `pacat` no `PATH`, além de uma sessão
PulseAudio/`pipewire-pulse` em execução. Execute estes testes **antes** de criar
os dispositivos para uso normal, em uma sessão sem endpoints Babel. Eles criam
e removem seus próprios módulos e recusam execução quando já existem endpoints
com os nomes reservados. Não os execute durante uma tradução.

```sh
cargo test --lib audio::linux::tests::live_virtual_routes_idempotence_interruption_and_cleanup -- --ignored --nocapture
cargo test --test live_engine -- --ignored --nocapture
```

Na máquina de desenvolvimento, as ferramentas Pulse foram extraídas localmente;
o comando usado foi:

```sh
PATH="$PWD/.tools/pulse/usr/bin:$PATH" cargo test --test live_engine -- --ignored --nocapture
```

O teste de baixo nível passou e confirmou criação idempotente, identidade dos
módulos pertencentes ao Babel, as duas pontes virtuais, interrupção de áudio
antigo com fila cheia e remoção dos módulos. Também verificou que os dispositivos
padrão da sessão não foram alterados.

O teste completo passou **três vezes consecutivas** após a correção de uma corrida
de cancelamento. O caminho exercitado é:

```text
tom sintético → babel_speaker → captura do Controller
            → provider loopback → babel_mic_bus → babel_microphone
            → captura de verificação
```

`loopback` é um provider de diagnóstico que transforma o formato de áudio; não
traduz nem acessa IA. O teste exige contadores de captura e saída positivos,
áudio recebido no outro extremo, parada sem erro e remoção dos endpoints.
Mesmo quando uma verificação falha, o teste para o Controller, cancela e aguarda
os processos auxiliares e tenta remover os módulos antes de devolver o erro.

Após a sequência, `ps -C parec -C pacat -o pid=,args=` não mostrou processos e
`pactl list short sinks` / `pactl list short sources` não mostraram endpoints
`babel_*`. Essa ausência vale para o encerramento dos testes; uma instalação
posterior destinada ao uso normal naturalmente mantém os dispositivos presentes.

## Gravação completa com os dispositivos de uso normal preservados

```sh
PATH="$PWD/.tools/pulse/usr/bin:$PATH" cargo test --test recorded_session original_tones_share_one_recorded_timeline_without_changing_babel_devices -- --ignored --nocapture
```

Esse teste cria quatro sinks nulos com nomes aleatórios próprios e usa dois
fluxos do Controller com provider loopback. Seus sinais são tons sintéticos de
440 Hz e 880 Hz. A remoção verifica os identificadores e a propriedade dos quatro
módulos temporários; o teste não chama instalação/desinstalação dos endpoints
Babel destinados ao usuário e pode coexistir com eles.

A execução real passou: gerou **um WAV de 3,53 s**, com as duas frequências na
mesma janela de um segundo e o nome de sessão aplicado ao padrão de arquivo.
A duração foi comparada com o relógio da sessão para detectar concatenação
indevida. O Controller parou sem erro, nenhum `parec`/`pacat` do teste permaneceu
e os endpoints Babel existentes eram os mesmos antes e depois.

O teste usa somente a configuração temporária e não captura dispositivos físicos.
Ele valida gravação e integração local; não mede qualidade de tradução por IA.

## Encaminhamento, gravação e transcrição independentes

```sh
PATH="$PWD/.tools/pulse/usr/bin:$PATH" cargo test --test recorded_session idle_routing_recording_and_transcription_are_independent_without_cloud -- --ignored --nocapture
```

Esse segundo cenário também usa quatro sinks temporários próprios e mantém os
endpoints Babel existentes. As duas opções de tradução ficam desativadas, e
nenhuma chave de IA é necessária. A execução real passou em **4,81 s**, verificando:

1. Encaminhamento dos dois tons originais enquanto não há sessão, sem criar o
   diretório de gravações.
2. Início de uma sessão somente de gravação: o áudio continua passando e um único
   WAV mistura os dois tons no mesmo intervalo, com contadores de tradução zerados.
3. Parada da sessão com retorno ao encaminhamento original e WAV finalizado de
   **2,21 s**, sem erro.
4. Início de uma sessão somente de transcrição usando um servidor HTTP local que
   simula whisper.cpp: **duas chamadas de reconhecimento**, um TXT com as duas
   origens e **zero chamadas de tradução ou síntese**. O áudio original continua
   passando, sem produzir outro WAV.
5. Parada da sessão, continuidade do áudio original e encerramento completo pelo
   Controller, sem deixar processos ou módulos temporários do teste.

As verificações medem a presença de 440 Hz e 880 Hz em novas capturas de cada
fase; áudio acumulado antes da transição é descartado pelo observador do teste.
A etapa de transcrição exercita o HTTP e a integração do Controller com uma
resposta simulada, sem executar um modelo de reconhecimento real. Nenhuma voz é
enviada à nuvem e nenhum dispositivo físico é aberto.

## Inicialização no login

Os testes de autostart usam entradas em diretórios temporários. A validação
adicional com `desktop-file-validate` e `gio launch` passou com Unicode, espaços,
percentuais, cifrões, crases, aspas e barras nos argumentos, incluindo espaço no
fim do diretório de trabalho. Ela inicia somente um gravador temporário de
argumentos; nenhuma entrada real de login foi ativada.

```sh
cargo test --lib autostart::tests::desktop_launcher_preserves_actual_arguments_through_gio -- --ignored --nocapture
```

Veja [autostart](autostart.md) para a integração e seus limites por sistema.

## Bandeja Linux

Foi iniciado um processo real de `target/debug/babel serve` com configuração
temporária e porta local livre. Foram verificados:

1. Registro de um novo item em `org.kde.StatusNotifierWatcher` via D-Bus.
2. Tooltip do item contendo “Babel”.
3. Resposta HTTP 200 do painel local.
4. Encerramento por SIGINT com código zero e retirada do item D-Bus.
5. Ausência de processos `parec`/`pacat` após a saída.

Para observar os registros em uma sessão compatível:

```sh
busctl --user get-property org.kde.StatusNotifierWatcher /StatusNotifierWatcher org.kde.StatusNotifierWatcher RegisteredStatusNotifierItems
cargo run -- serve
```

Compare a propriedade D-Bus antes e depois do lançamento, e novamente após
Ctrl+C. O teste confirmou o ciclo de registro/encerramento; não comprovou a
aparência visual nem cada item do menu em todos os ambientes desktop. Alguns
ambientes precisam de suporte a StatusNotifier/AppIndicator. O painel continua
disponível se a bandeja não puder iniciar; `--no-tray` desativa a tentativa.

A recuperação da bandeja também passou em um barramento D-Bus isolado, sem
`StatusNotifierWatcher` e sem abrir dispositivos de áudio:

```sh
dbus-run-session -- cargo test --lib linux_tray_worker_waits_for_desktop -- --ignored --nocapture
```

O worker permaneceu vivo atravessando a tentativa de registro após três segundos,
emitiu um único aviso e encerrou em menos de um segundo após cancelamento. A
criação do ícone usa a API síncrona fora de um runtime Tokio; somente a espera é
assíncrona. O teste não desbloqueia a tela nem altera extensões do desktop.

## Windows e macOS

```sh
rustup target add x86_64-pc-windows-gnu
cargo check --all-targets --target x86_64-pc-windows-gnu
```

O cross-check Windows passou, incluindo CPAL, os providers e o loop de eventos
nativo da bandeja. Ele verifica tipos e compilação; não executa o programa nem
confirma instalação dos drivers, captura, reprodução, permissões ou comportamento
da bandeja em Windows. Não foi realizado teste físico em Windows ou macOS, nem
build macOS neste host Linux.

A revisão dos workers CPAL verificou filas de tamanho limitado, callbacks sem
alocação/rede/locks, sinalização atômica de falha, verificação frequente de
cancelamento e timeout quando o dispositivo deixa de consumir amostras. O guard
de encerramento usa um token filho: abortar a tarefa assíncrona solicita o
cancelamento do worker sem cancelar o supervisor e ocultar um erro do dispositivo.
Uma chamada do sistema operacional travada não pode ser interrompida por esse
token. Um registro por identidade/direção impede reabrir o mesmo endpoint até
o worker anterior realmente liberar o recurso; cancelar a tarefa não equivale
a comprovar que o driver fechou o stream. Os drivers e as dependências nativas
permanecem fora da garantia de `forbid(unsafe_code)` do código do projeto.

Os testes dos monitores usam modelos/fixtures para verificar identidade,
direção, exclusão do processo Babel e falhas que fecham a rota. Os testes do
supervisor verificam espera, cancelamento, descarte de áudio antigo, troca do
físico e continuidade da sessão. Eles não simulam integralmente as políticas
de privacidade do macOS, a enumeração de sessões de cada aplicativo Windows
nem o comportamento de um driver real.

### Roteiro de validação no sistema de destino

Use macOS 14.2+ ou Windows 10/11 com os dois cabos próprios Babel instalados
conforme [drivers nativos](native-drivers.md). Cabos BlackHole/VB-CABLE são
alternativas opcionais, com sua instalação própria. Registre versão do
SO, arquitetura, commit, driver, dispositivo físico, aplicativo de chamada e
permissões concedidas. Comece com tradução e transcrição desligadas, usando
áudio sintético e um aplicativo de teste que permita selecionar entrada/saída
independentemente. Use fones se testar microfone físico depois.

| Cenário | Resultado a verificar |
|---|---|
| Nenhum aplicativo usando os cabos | Ambas as rotas aguardam; nenhum stream físico do Babel aberto, medidores instantâneos zerados. |
| Aplicativo captura somente a ponta do mic virtual | Só o microfone abre; áudio chega à captura do aplicativo e a saída do Babel continua esperando. |
| Aplicativo reproduz somente no cabo de saída | Só a saída abre; áudio chega ao destino físico escolhido. |
| Ambas as pontas em uso | Duas rotas independentes, sem realimentação ou duplicação de áudio. |
| Aplicativo troca a saída para o físico, mantendo o mic virtual | A rota de saída fecha; o mic permanece ativo. Retornar ao cabo retoma a saída sem tocar áudio acumulado. |
| Aplicativo troca o mic para o físico, mantendo a saída virtual | A rota do mic fecha; saída permanece ativa. Comandos de voz do Babel não podem disparar por áudio de saída. |
| Apenas o padrão global do SO muda | Aplicativo explicitamente conectado ao cabo continua atendido; padrão global não substitui a identidade configurada. |
| Físico trocado pelo painel/bandeja em sessão ativa | Novo físico recebe a rota; sessão e arquivos permanecem os mesmos, sem replay do backlog. |
| Físico desconectado e reconectado, ou segundo físico escolhido | Diagnóstico e tentativa de recuperação da identidade selecionada; nenhum fallback silencioso para o padrão. |
| Sessão com gravação original, tradução/transcrição desligadas | Um WAV contém as duas origens na mesma linha do tempo; pausa/retomada mantém o ID e o arquivo. |
| Falta de permissão/API, par Windows ambíguo ou driver não reconhecido | A direção afetada permanece fechada e informa o problema. Erro global de enumeração pode fechar ambas. |
| Sair e iniciar novamente | Streams e workers do processo anterior terminam; seleção persistente resolve os mesmos endpoints. |

Confira atividade tanto no painel quanto na ferramenta de áudio do sistema;
medidor em zero sozinho não comprova que a captura foi fechada. No macOS,
selecione cada dispositivo Babel diretamente, sem Aggregate/Multi-Output. No Windows,
comece em WASAPI compartilhado e registre se uma sessão nova permanece em
espera; não trate ASIO, Kernel Streaming ou modo exclusivo como validados.

Salve evidências por cenário: horário, estado de cada rota, ID da sessão,
endpoint selecionado e duração/amostras do WAV sintético quando aplicável.
Não inclua tokens, chaves de API ou voz pessoal nos artefatos de teste.
Marque cada linha como passou, falhou ou não executada e anexe o diagnóstico
quando falhar. Esta tabela é um roteiro pendente em hardware de destino, não
um registro de execução já concluída.

## O que ainda exige execução no ambiente de destino

- Chamadas autenticadas reais a Gemini, OpenAI e ElevenLabs. Os testes descritos
  não consumiram créditos nem enviaram voz a esses serviços.
- Inferência com modelos reais de whisper.cpp, Ollama e Piper, incluindo consumo
  de RAM/VRAM, qualidade por par de idiomas e velocidade no hardware escolhido.
- Carregamento/instalação dos drivers próprios Babel (ou cabos externos opcionais),
  assinaturas, Driver Verifier no Windows, permissões de microfone,
  desconexão física e recuperação de dispositivos.
- Medições de latência fim a fim, jitter, CPU/RAM e estabilidade prolongada sob
  carga. Filas limitadas impedem crescimento ilimitado do backlog; isso não é um
  benchmark que comprove desempenho “extremo”.

Veja [plataformas](platforms.md) para a topologia e os drivers e
[outros providers](other-providers.md) para provisionar modelos e serviços.


## Drivers próprios: testes separados do aplicativo

Os workspaces `native/macos`, `native/windows/transport` e
`native/windows/installer` não herdam nem removem o `forbid(unsafe_code)` do
aplicativo. O transporte seguro tem seus testes; FFI, SDK e WDK têm validação
separada. Execute no checkout:

```sh
cargo test --locked --manifest-path native/macos/Cargo.toml
cargo test --locked --manifest-path native/windows/transport/Cargo.toml --lib
cargo test --locked --manifest-path native/windows/installer/Cargo.toml
python3 -m unittest discover -s native/windows/tests -p 'test_*.py' -v
cargo test --locked --example native_driver_smoke
```

O primeiro comando no Linux testa o núcleo e o contrato Rust; não compila a ponte
com o SDK Apple. O transporte Windows testa frames PCM, limite de memória,
isolamento e descarte ao parar uma ponta. Seu harness C++/Rust usa stubs WDK
apenas em testes e não valida IRQL/DPC reais. O instalador tem testes portáveis
para recusar identidade parecida e caminhos OEM que escapem do pacote.

A checagem de compilação cruzada cobre também o instalador:

```sh
cargo clippy --locked --all-targets --target x86_64-pc-windows-gnu -- -D warnings
cargo clippy --locked --all-targets --target x86_64-pc-windows-gnu \
  --manifest-path native/windows/installer/Cargo.toml -- -D warnings
```

`.github/workflows/native-drivers.yml` separa os testes portáveis, build com SDK
macOS, build WDK x64/ARM64 e empacotamento dos três sistemas. Usa runners hospedados
e versões/hashes fixados dos pacotes NuGet oficiais do SDK/WDK. O CI não carrega
drivers nem abre áudio; somente o instalador do aplicativo Windows x64 é exercitado
em uma pasta temporária do runner. Os artefatos de desenvolvimento são identificados
como tal. Ter o workflow no repositório não é evidência de sua execução.

Depois de compilar, assinar e instalar no sistema de destino, execute o smoke
sintético descrito em [drivers nativos](native-drivers.md#verificação-do-transporte-instalado).
Ele exige quatro IDs de dispositivos e nunca escolhe os padrões do sistema.
Teste também múltiplos leitores/aplicativos, suspensão/retomada do SO, troca de
físico durante sessão, desconexão, reinstalação, remoção e reinicialização. No
Windows, execute Driver Verifier/validação WDK em máquina de teste e verifique
notificações, cancelamento de DPC e retorno de áudio antes de produção. No macOS,
valide o bundle no CoreAudio real, privacidade de microfone, clock e tamanhos de
buffer. Nenhum desses resultados pode ser inferido de testes de fila em Linux.
