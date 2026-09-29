# Comandos de voz locais com Needle 3 e MCP

O agente usa uma cópia do **microfone físico original que alimenta o Babel**. Funciona durante o encaminhamento de áudio original e durante sessões de tradução, transcrição ou gravação. Não abre outro microfone, não ouve a saída dos interlocutores e não depende de uma sessão de gravação. A opção `agent.enabled` vem ligada. Depois da instalação dos serviços locais descrita abaixo, o Babel pode iniciá-los e descobrir suas portas automaticamente. A escuta só acontece enquanto um aplicativo usa o microfone virtual Babel; a rota de saída nunca ativa o agente.

Diga **“Babel, acenda a luz da cozinha”**, ou diga **“Babel”**, espere a indicação de ativação e então dê o comando. O nome é configurável. A comparação ignora maiúsculas/minúsculas, exige palavras completas e aceita um cumprimento inicial como “Oi, Babel” ou “Hey, Babel”. Uma menção no meio de uma conversa (“eu uso o Babel”) não ativa ferramentas. Depois de uma ativação sem comando, o prazo padrão para a próxima fala é oito segundos. “Babel, cancelar” cancela essa ativação; durante uma chamada em processamento, use o botão de cancelar no painel.

## Fluxo

1. O worker de áudio copia quadros para uma fila limitada, sem aguardar a IA.
2. Um worker separado converte para PCM mono de 16 kHz e separa falas por energia e silêncio.
3. O servidor **whisper.cpp local** transcreve a fala original, sem tradução. O Babel procura o nome de ativação no texto.
4. O Babel reúne as ferramentas das integrações MCP habilitadas e envia somente o comando e os esquemas ao **Needle 3 local**.
5. Needle escolhe ferramentas e preenche argumentos. O Babel verifica confiança, recusa, identificação exata, campos sem suporte na fala e os JSON Schemas antes de executar.
6. O painel mostra ativação, transcrição da fala seguinte, decisão, execução, sucesso ou falha. Resultados são texto/JSON das ferramentas, não uma resposta falada inventada.

O agente executa no máximo quatro chamadas por comando por padrão, em sequência. Todas passam por validação antes da primeira execução; cada integração é validada novamente antes da chamada. Uma falha interrompe as chamadas seguintes, sem desfazer aquelas que já ocorreram. Timeout ou cancelamento **não repetem uma ferramenta**: o servidor pode já ter concluído a ação. Configurações alteradas, troca de microfone, desligamento do agente e encerramento do app cancelam trabalho pendente e descartam a fala antiga.

Enquanto Needle/MCP processa um comando, novas falas para o agente são descartadas para impedir uma fila de ações atrasadas. O áudio normal da conversa continua encaminhado. Os comandos também permanecem no áudio encaminhado; este recurso não silencia a palavra de ativação na chamada.

## Serviços gerenciados e portas dinâmicas

O Babel usa **dois processos locais separados**: whisper.cpp reconhece a fala e
o nome de ativação; Needle bridge escolhe as ferramentas MCP. Tradução por
Gemini/OpenAI não substitui esses serviços. Ambos executam no computador do
Babel, mesmo quando o painel é aberto em outro computador.

Em **Comandos → Serviços locais de fala e decisão**, mantenha os dois endpoints
como `auto`. O Babel inicia os helpers já instalados com `--port 0`: o sistema
operacional escolhe uma porta livre e mantém o socket reservado. Cada processo
anuncia sua identidade e seu endpoint real por uma linha `BABEL_SERVICE_READY`
seguida de JSON. O Babel valida esse anúncio antes de usar o serviço; não tenta
adivinhar uma porta nem se conectar a um número padrão.

Os endpoints efetivos aparecem no **status da página Comandos**. São dados da
execução atual: não copie essas portas para a configuração gerenciada. O TOML
continua com `whisper_endpoint = "auto"` e `needle_endpoint = "auto"`, inclusive
quando um helper reinicia e recebe outra porta.

O campo **Pasta dos serviços locais** corresponde a `agent.services_directory`.
Vazio, usa a pasta absoluta do arquivo TOML carregado. Para uma instalação em
outro lugar, informe a pasta absoluta do Babel que contém `scripts` e `.tools`;
não informe apenas a pasta do binário ou do modelo. Caminhos relativos não são
aceitos nesse campo. Essa pasta é independente de `files.base_path`, que
controla os arquivos de transcrição e gravação.

Estrutura esperada dentro dessa pasta:

```text
scripts/needle_bridge.py
.tools/needle/bin/python                         # Linux/macOS
.tools/needle/Scripts/python.exe                 # Windows
.tools/whisper.cpp/models/ggml-base.bin
.tools/whisper.cpp/build/bin/whisper-server       # Linux/macOS
.tools/whisper.cpp/build/bin/whisper-server.exe   # Windows, configuração única
.tools/whisper.cpp/build/bin/Release/whisper-server.exe  # Windows, Visual Studio
```

Os helpers são iniciados quando o agente habilitado precisa atender ao microfone
virtual em uso. Ao deixar de usar esse microfone, o Babel pausa a escuta e
descarta falas pendentes; mantém os processos já iniciados para reutilização.
Desabilitar o agente ou sair normalmente do aplicativo encerra os helpers que
ele iniciou. Fechar apenas a janela de configurações não encerra o app. Com o
tray configurado para iniciar no login, a mesma política vale na próxima
execução; não é necessário criar um serviço separado para os helpers gerenciados.

O app não instala pacotes, baixa Whisper nem compila ferramentas ao abrir as
configurações. Prepare os binários, o ambiente Python e os modelos explicitamente
antes do uso, com as instruções seguintes. Instalação ausente produz um
diagnóstico em Comandos e não interrompe o encaminhamento normal de áudio.

Um endpoint HTTP(S) local escrito explicitamente no lugar de `auto` seleciona
um **serviço externo ao gerenciamento do Babel**. Nesse caso, você administra a
inicialização, a porta e o encerramento dele. Os dois campos são independentes:
é possível gerenciar um helper e usar outro externamente. O endpoint de
reconhecimento do provider Local, na página **Tradução e vozes**, também é uma
configuração separada e não acompanha automaticamente o endpoint do agente.

## Instalar Whisper local

O [instalador do Babel](../scripts/setup_whisper.py) usa o servidor HTTP oficial
[whisper.cpp v1.9.4](https://github.com/ggml-org/whisper.cpp/releases/tag/v1.9.4),
fixa o commit `927cfce34f31707e17f2bff35c349632fb9e2c3a` e baixa o modelo `base`
multilíngue, de aproximadamente 148 MB. Verifica o SHA-256 do modelo antes de
usá-lo e aplica o [patch de descoberta da porta](../scripts/patches/whisper-dynamic-port.patch).
Executá-lo novamente reaproveita a instalação compatível. Um checkout alterado
ou modelo divergente é preservado e gera erro em vez de ser sobrescrito.

Execute os comandos a partir da pasta do repositório Babel. O instalador usa
somente a biblioteca padrão do Python; requer **Python 3.9+, Git, CMake e um
compilador C/C++**. Não inicia o servidor nem modifica a configuração do Babel.

### Linux

Tenha um compilador C/C++ e um gerador de build, como Make ou Ninja, instalados.
Em Debian/Ubuntu, os requisitos geralmente vêm de `python3`, `git`, `cmake` e
`build-essential`; a instalação desses pacotes é administrada pelo usuário.

```bash
python3 scripts/setup_whisper.py --backend cpu --jobs 4
```

Para NVIDIA com toolkit CUDA e compilador compatível já instalados:

```bash
python3 scripts/setup_whisper.py --backend cuda --jobs 4
```

O instalador não instala CUDA. Ter somente o driver da GPU não basta para
compilar esse backend.

### macOS

Tenha Python, Git, CMake e as ferramentas de compilação do Xcode disponíveis.
Para CPU:

```bash
python3 scripts/setup_whisper.py --backend cpu --jobs 4
```

Para aceleração Metal em hardware e ferramentas compatíveis:

```bash
python3 scripts/setup_whisper.py --backend metal --jobs 4
```

O servidor recebe WAV por HTTP local; a permissão de captura do microfone é do
Babel. Estes comandos e a inferência não foram validados em hardware macOS neste
ambiente.

### Windows

Tenha Python, Git, CMake e Visual Studio Build Tools com suporte a C/C++.
Use um terminal de desenvolvimento da arquitetura do computador. No PowerShell:

```powershell
py -3 scripts/setup_whisper.py --backend cpu --jobs 4
```

Com toolkit CUDA e compilador compatível instalados, use `--backend cuda`.
Com o gerador Visual Studio, o executável costuma ficar em `build/bin/Release`;
geradores de configuração única podem usar `build/bin`. O instalador informa o
caminho final e o Babel procura ambas as formas. Mantenha as DLLs produzidas
junto ao executável. Não é necessário WSL. Build e inferência nativos no Windows
ainda não foram validados neste ambiente.

Todos os builds usam otimizações para a CPU da máquina (`GGML_NATIVE=ON`). Não
transfira esse binário para outra CPU sem conferir compatibilidade. `--jobs`
limita o paralelismo da compilação, não o tempo de resposta da inferência. O
helper gerenciado é iniciado com quatro threads de inferência.

### Execução manual e verificação sem áudio

Normalmente basta instalar e deixar o endpoint em `auto`. Para diagnóstico de
uma instalação externa, inicie manualmente o **binário com o patch do Babel**:

```bash
.tools/whisper.cpp/build/bin/whisper-server -m .tools/whisper.cpp/models/ggml-base.bin -t 4 -l auto --host 127.0.0.1 --port 0
```

No Windows, use o executável `.exe` indicado pelo instalador. Leia a linha
`BABEL_SERVICE_READY` e obtenha `endpoint` do JSON. O campo `port` contém o número
atribuído pelo SO; não use `:0` como URL de conexão. O servidor original v1.9.4
sem o patch não publica esse contrato de descoberta. Não use `--no-prints` nem
`--no-context`: essas opções não são aceitas pelo servidor dessa versão.

Para validar sem enviar áudio, defina `WHISPER_ENDPOINT` com o endpoint real
anunciado e execute em Linux/macOS:

```bash
curl --max-time 5 -i "${WHISPER_ENDPOINT%/inference}/health"
curl --max-time 5 -i -F response_format=json "$WHISPER_ENDPOINT"
```

No PowerShell, atribua o endpoint anunciado a `$WhisperEndpoint` e use:

```powershell
curl.exe --max-time 5 -i ($WhisperEndpoint -replace '/inference$', '/health')
curl.exe --max-time 5 -i -F response_format=json $WhisperEndpoint
```

O primeiro pedido precisa retornar HTTP 200, `Server: whisper.cpp` e JSON com
`status: ok`. O segundo não inclui arquivo de áudio: o resultado esperado é
HTTP 400 com `Invalid request` ou o JSON oficial indicando ausência de `file`.
Um `/health` genérico que responde 200 não confirma a identidade do serviço.

O Babel faz essas duas verificações antes de transmitir uma fala. A confirmação
é reutilizada durante a mesma configuração/rota e invalidada após falha de
transcrição. Se usar proxy local num endpoint explícito, preserve o cabeçalho e
os caminhos: `/asr/inference` exige `/asr/health`. Respostas de inferência
precisam ter `Content-Type: application/json`.

Depois da verificação, o Babel envia WAV PCM16 mono/16 kHz em multipart,
`response_format=json`, `translate=false` e o idioma selecionado. `auto`
identifica o idioma; `pt` e `en` o fixam. Para português, use modelo multilíngue:
as variantes `.en` atendem inglês.

O Whisper gerenciado não usa chave de API; mantenha `whisper_api_key_env` vazio.
Para um servidor externo autenticado, configure um endpoint explícito e informe
nesse campo o **nome** de uma credencial disponível ao Babel. O segredo permanece
na memória do app ou no ambiente, nunca no TOML.

Whisper também reconhece a palavra inicial. Esta implementação não usa um
pequeno detector acústico dedicado: cada trecho com voz passa pelo ASR local.
Consumo e demora dependem de CPU/GPU, modelo, duração e ruído. Sem Whisper, o
áudio principal continua funcionando. O modo gerenciado descarta stdout e
stderr dos helpers, exceto o anúncio necessário à descoberta; não salva falas
nesses logs. Em execução manual, não ative `--print-realtime`,
`--print-progress` ou depuração se não quiser expor conteúdo no terminal.

### Interpretar os diagnósticos de Whisper

| Diagnóstico nos detalhes | Significado e ação |
| --- | --- |
| `Whisper is not installed in the local services folder` | Confira a pasta dos serviços e execute o instalador nela. |
| `Whisper model is missing` | Execute novamente o instalador; ele verifica o modelo antes de usá-lo. |
| `Local voice service did not announce its bound port` | Confira o build com o patch do Babel, o modelo e os requisitos do runtime. Um servidor original sem o marcador não atende ao modo gerenciado. |
| `Whisper local service is unavailable or timed out` | Confira o estado do processo e o timeout. Em modo externo, use o endpoint efetivamente anunciado pelo processo atual. |
| `Whisper endpoint is not a verified whisper.cpp server` | A resposta não identifica o protocolo esperado: pode ser outro aplicativo, caminho incorreto ou proxy removendo cabeçalhos. Nenhum áudio é enviado nessa verificação. |
| `Whisper model is not ready` | O serviço identificado ainda não respondeu com sucesso ao health check; confira a instalação e o arquivo do modelo. |

Após corrigir uma instalação, o Babel tenta iniciar/usar os helpers novamente
enquanto o agente está habilitado e o microfone virtual em uso. A página de
configurações apenas consulta o estado: não captura uma gravação de teste nem
executa ferramentas MCP por conta própria.

Falhas de ASR anteriores ao nome de ativação aparecem como diagnóstico na aba
**Comandos**, sem aviso flutuante ou notificação de comando. Falhas após uma
ativação mantêm o retorno visual correspondente. A API distingue os casos por
`error_scope: "service"` ou `"command"`.

## Needle 3 local

O modelo correto desta integração é [Cactus Compute Needle 3](https://huggingface.co/Cactus-Compute/needle3). Ele usa seu próprio runtime `.cact`; não se deve tratá-lo como um modelo GGUF de Ollama/llama.cpp. A [API Python oficial](https://github.com/cactus-compute/needle/blob/main/llms.txt) recebe esquemas em `Needle(tools=...)` e devolve chamadas e argumentos com `complete(...)`.

O projeto inclui **`scripts/needle_bridge.py`**, um adaptador HTTP do Babel sobre essa API. `/complete` é um protocolo deste adaptador, não uma API HTTP oficial do Needle. O programa roda em um processo separado e usa apenas `complete`, nunca `run`: quem executa MCP é o cliente Rust do Babel.

Crie o ambiente Python na pasta dos serviços. No Linux e macOS:

```bash
python3 -m venv .tools/needle
.tools/needle/bin/python -m pip install cactus-needle==3.0.6
.tools/needle/bin/python -c "import os; os.environ['NEEDLE_TELEMETRY']='0'; os.environ['DO_NOT_TRACK']='1'; from needle import Needle; Needle(tools=[]).close()"
```

O pacote Python `cactus-needle==3.0.6` seleciona o motor nativo **3.0.2**. Os arquivos necessários estão [publicados no repositório oficial](https://huggingface.co/Cactus-Compute/needle3/tree/main/python), e o [seletor de plataformas do SDK](https://github.com/cactus-compute/needle/blob/main/needle/agent/fetch.py) inclui Windows nativo:

| Sistema / arquitetura | Motor fornecido | Verificação no Babel |
| --- | --- | --- |
| Linux x86_64, glibc | `manylinux2014_x86_64`, biblioteca `.so` | Inferência real testada neste host |
| Linux ARM64, glibc | `manylinux2014_aarch64`, biblioteca `.so` | Arquivo oficial disponível; não executado aqui |
| Linux x86_64 / ARM64, musl | `musllinux_1_2_x86_64` / `musllinux_1_2_aarch64` | Arquivos oficiais disponíveis; não executados aqui |
| macOS 11+, Apple Silicon / Intel | `macosx_11_0_arm64` / `macosx_11_0_x86_64`, biblioteca `.dylib` | Arquivos oficiais disponíveis; não executados aqui |
| Windows x86_64 / ARM64 | `win_amd64` / `win_arm64`, biblioteca `.dll` | SDK e arquivos nativos disponíveis; não executados aqui |

No Windows, use Python da arquitetura correspondente e estes comandos no PowerShell; não é necessário WSL para o runtime publicado:

```powershell
py -3 -m venv .tools\needle
.tools\needle\Scripts\python.exe -m pip install cactus-needle==3.0.6
.tools\needle\Scripts\python.exe -c "import os; os.environ['NEEDLE_TELEMETRY']='0'; os.environ['DO_NOT_TRACK']='1'; from needle import Needle; Needle(tools=[]).close()"
```

O último comando prepara o motor nativo e os pesos oficiais no cache do usuário
usando a API Python. Pode baixar arquivos nessa etapa explícita; não transcreve
áudio nem executa ferramentas. Faça a preparação com o mesmo usuário que inicia
o Babel e antes de depender dos comandos numa chamada. Somente instalar o
pacote `cactus-needle` não garante que o motor e os pesos já estejam disponíveis.
No modo gerenciado, o Babel inicia o Needle com `HF_HUB_OFFLINE=1`: se faltar
algo no cache, a solicitação falha sem iniciar um download. Conclua a preparação
explícita acima e tente novamente. O cache pertence ao usuário do processo;
prepará-lo em outra conta não instala os pesos para a conta que executa o Babel.

Com os arquivos preparados, deixe `needle_endpoint = "auto"`: o Babel inicia
`needle_bridge.py --port 0`. O adaptador se vincula somente a `127.0.0.1` e
anuncia o endpoint real em `BABEL_SERVICE_READY`, sem porta fixa. `/health`
confirma a identidade do bridge; o carregamento do modelo ocorre na primeira
solicitação de planejamento, por isso essa etapa ainda pode demorar.

Para administrar o helper separadamente, por exemplo com pesos próprios:

```bash
.tools/needle/bin/python scripts/needle_bridge.py --port 0 --weights /caminho/needle3.cact
```

Copie o `endpoint` anunciado para o campo do Needle somente nesse modo externo.
No Windows, substitua o Python pelo de `.tools\needle\Scripts\python.exe`.
WSL2 pode hospedar um helper Linux externo, sujeito ao encaminhamento de
[localhost do WSL](https://learn.microsoft.com/en-us/windows/wsl/networking#accessing-linux-networking-apps-from-windows-localhost).
Esse arranjo não foi testado aqui e não é iniciado pelo gerenciador nativo do
Babel. Use o endpoint loopback real acessível no Windows; endereços privados da
VM são recusados pela política de inferência local.

O modelo permanece carregado. O adaptador reutiliza o catálogo quando ele é igual e reinicia o histórico de cada comando. A telemetria opcional do runtime é desabilitada por `NEEDLE_TELEMETRY=0` e `DO_NOT_TRACK=1`. O motor nativo do Needle fica fora do processo Rust; isso isola sua memória do roteamento de áudio, sem afirmar que dependências nativas sejam escritas em Rust.

Para autenticar o adaptador, configure `needle_api_key_env` com uma referência de credencial, por exemplo `BABEL_NEEDLE_TOKEN`. No modo gerenciado, o Babel passa essa credencial ao próprio helper. No modo externo, disponibilize o mesmo segredo aos dois processos e inicie o bridge com `--api-key-env BABEL_NEEDLE_TOKEN`. O adaptador rejeita requisições originadas diretamente por páginas do navegador; apenas o backend local faz as chamadas.

Inferência de voz aceita somente HTTP(S) em endereço literal de loopback ou `localhost`, sem proxy nem redirecionamentos. Integrações MCP têm suas próprias regras e podem acessar serviços remotos autenticados.

## Configuração

```toml
[agent]
enabled = true
desktop_notifications = true
wake_name = "Babel"
whisper_endpoint = "auto"
whisper_language = "auto"
whisper_api_key_env = ""
needle_endpoint = "auto"
needle_api_key_env = ""
services_directory = ""
min_confidence = 0.85
max_calls = 4
silence_ms = 600
max_utterance_ms = 10000
command_window_secs = 8
timeout_secs = 20
vad_threshold = 0.012
```

- `whisper_endpoint` e `needle_endpoint`: `auto` para helpers gerenciados e portas escolhidas pelo SO; URL explícita para serviço externo local. Portas descobertas aparecem no status, sem substituir `auto` na configuração.
- `services_directory`: vazio usa a pasta absoluta do TOML carregado; um valor personalizado deve ser absoluto e conter a estrutura de instalação acima. Não depende da pasta de onde o app foi iniciado.
- `min_confidence`: limiar de execução, de 0 a 1. Needle sem confiança numérica, com `suppressed_calls`, com `validation.ungrounded`, com negação sinalizada ou sem chamadas resulta em recusa. O intervalo 0–1 ajusta a política do Babel; o runtime possui sua própria recusa interna, documentada para confiança abaixo de 0,1 e falhas de fundamentação. `complete()` não expõe um parâmetro para desligar essa recusa, e chamadas em `suppressed_calls` não são promovidas a execução. Um limiar é uma escolha do aplicativo, não garantia de acerto.
- `max_calls`: de uma a oito chamadas por comando. O padrão é quatro.
- `silence_ms`: silêncio que encerra uma fala, de 200 a 2000 ms. Uma pausa mais curta melhora a resposta, mas pode dividir frases naturais.
- `max_utterance_ms`: limite de fala, de 1000 a 15000 ms. Falas acima dele são descartadas em vez de executar um comando cortado.
- `command_window_secs`: prazo após dizer somente o nome, de dois a trinta segundos.
- `timeout_secs`: limite para cada requisição/etapa de rede, de um a 120 segundos.
- `vad_threshold`: energia mínima normalizada, de 0,0001 a 0,5. Aumentar reduz ruído e pode perder fala baixa.
- `desktop_notifications`: aviso também fora do painel, sujeito ao suporte e às permissões do desktop.

A interface guarda as referências de credenciais e as integrações na configuração do agente. Os valores de chaves devem ser fornecidos pelo painel ou ambiente. Os textos da interface seguem o idioma do aplicativo; `whisper_language` controla somente o reconhecimento da voz.

## Integrações MCP e autenticação

Adicione várias integrações no painel de configurações do agente, habilitando somente aquelas cujas ferramentas deseja disponibilizar. O catálogo é construído a partir de `tools/list`; o modelo não inventa nomes nem executa comandos de shell diretamente. Duas ferramentas com o mesmo nome em servidores diferentes recebem identidades distintas. Quando não há integrações habilitadas, nenhuma ação externa pode ser executada.

Para um servidor **stdio**, informe executável e argumentos separadamente, diretório de trabalho se necessário e variáveis de ambiente. Use referências a credenciais para valores secretos. O Babel inicia o executável diretamente, sem interpolar uma linha em shell.

Para um servidor **HTTP MCP**, informe seu endpoint Streamable HTTP e selecione autenticação sem chave, Bearer ou OAuth conforme o servidor. Bearer usa uma referência de credencial; OAuth usa o fluxo de conectar/autorizar do painel, com os scopes e client ID/client secret que o servidor exigir. Não confunda a autenticação MCP com a chave do adaptador Needle. A disponibilidade de uma ferramenta depende da integração conectada e das permissões concedidas pelo provedor.

As ferramentas e seus resultados são dados. Resultados de uma ferramenta não viram instruções para novas ações autônomas: esta versão executa o plano da fala inicial e exibe o resultado. Planos que dependem do resultado de uma ferramenta anterior exigem um novo comando do usuário.

## Limites e operação

- O reconhecimento ocorre após silêncio e inferência local; não há promessa de despertar instantâneo. Voz baixa, fala simultânea, sotaques e ruído podem produzir transcrição incorreta. A ativação não identifica nem autentica o locutor.
- Não há filtro acústico de eco nem autenticação por voz. Áudio reproduzido perto do microfone pode ser captado por ele, embora o Babel nunca envie diretamente a sua rota de saída ao agente.
- Os esquemas ajudam a restringir argumentos, mas nenhum modelo garante intenção correta. Verifique a qualidade do Needle com suas ferramentas e idiomas. Pesos personalizados sem confiança calibrada são recusados pela política atual.
- O catálogo aceita até 128 ferramentas e até 256 KiB de requisição. O Needle possui recuperação interna para catálogos grandes; descrições claras e catálogos menores facilitam escolhas corretas.
- O agente descarta áudio com mais de 500 ms na fila e segmentos antigos, mantém somente um segmento aguardando ASR, limita resposta JSON a 256 KiB e nunca bloqueia a rota de áudio esperando IA.
- Ativar o agente não ativa salvamento de transcrição ou gravação. A transcrição temporária de reconhecimento fica em memória; só o comando e o resultado atual aparecem no status. O adaptador não registra texto/argumentos em logs.
- Fechar o painel não encerra o Babel nem seu agente. Sair normalmente encerra roteamento, escuta e os helpers gerenciados. Servidores configurados por endpoint explícito continuam sob administração externa.

## Verificação de desenvolvimento

```bash
cargo test --lib commands::
python3 -m unittest discover -s scripts -p test_needle_bridge.py -v
python3 -m unittest discover -s scripts -p test_setup_whisper.py -v
```

A suíte usa áudio sintético e servidores locais simulados para verificar palavra inteira/endereço, fala em duas etapas, recusa, argumentos inválidos, cancelamento, troca de microfone, limites de fila e contrato do helper. Esses testes não executam ações em contas reais nem medem a acurácia de um modelo carregado.

### Inferência real verificada neste ambiente

Além dos testes simulados, foi carregado o pacote oficial `cactus-needle==3.0.6` em um ambiente isolado e chamado o `Planner.complete` do helper com uma única ferramenta fictícia de consulta de clima. Nenhuma ferramenta foi executada. O modelo produziu corretamente `city=Lisbon` para a pergunta em inglês e `city=Lisboa` para a pergunta em português. A confiança foi 0,9417 em inglês e 0,5413 em português; portanto, **o limiar padrão de 0,85 recusaria esse exemplo em português**. Isso demonstra a integração real e uma limitação concreta de acurácia/confiança, não uma certificação dos idiomas.

A chamada inicial levou 13,751 segundos incluindo preparação/download/carregamento, e a chamada seguinte em português levou 4,355 segundos neste host. O runtime relatou pico de RAM de 153,6 MB. Esses valores dependem da máquina e do catálogo; não são uma promessa de latência. A capacidade de manter áudio encaminhado não depende dessa inferência. Ações em serviços MCP externos continuam dependendo das integrações e credenciais do usuário.

O Whisper v1.9.4 com o patch foi compilado e testado neste Linux com CPU
Intel i7-12700H, quatro threads e modelo `base` multilíngue. O servidor recebeu
uma porta do SO, publicou o marcador e passou pelos dois testes de identidade.
Transcreveu corretamente o sample público JFK incluído no projeto, sem capturar
o microfone: um trecho de quatro segundos levou 4,81 s na primeira inferência e
3,46 s na seguinte; o sample de onze segundos levou 3,30 s. O RSS observado foi
aproximadamente 245 MiB. Isso demonstra execução real e também um atraso
perceptível em CPU; não mede precisão para todo idioma nem comprova desempenho
em macOS, Windows ou GPU.
