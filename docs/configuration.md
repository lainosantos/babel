# Configuração e operação

## Navegação do painel

O painel separa os controles em seis páginas:

- **Roteamento:** escolha o microfone e a saída físicos e os dispositivos virtuais
  de cada direção. O encaminhamento do áudio original funciona sem iniciar uma
  sessão, enquanto um aplicativo usa o dispositivo virtual correspondente.
- **Tradução e vozes:** ative a tradução de cada direção, escolha idiomas,
  provedores e vozes e configure os perfis e as credenciais de IA compartilhados.
  A biblioteca de vozes fica nesta página.
- **Transcrição:** escolha as origens do texto original, os horários opcionais
  e a pasta do TXT, e acompanhe os últimos trechos recebidos. O atalho **Provedor
  de reconhecimento** leva às configurações de IA em **Tradução e vozes**.
- **Gravação:** escolha as origens do áudio original e a pasta do WAV único.
- **Comandos:** configure a ativação pelo microfone, os serviços locais e as
  integrações MCP.
- **Ajustes:** configure a pasta base e o padrão de nomes compartilhados,
  a qualidade do áudio, os dispositivos virtuais e o início automático.

**Transcrição** e **Gravação** têm o atalho **Pasta base e nomes de arquivos** para
**Ajustes → Arquivos da sessão**. Os recursos continuam independentes: é possível
gravar ou transcrever sem ativar tradução. A transcrição usa o provedor escolhido
para cada direção; seus perfis e credenciais são os mesmos de **Tradução e vozes**.
Mudar de página preserva os ajustes ainda não salvos.

## Idioma da interface

O seletor **Idioma da interface / Interface language** altera o painel e os menus
da bandeja. As opções são **Padrão do sistema**, **English** e **Português**.
O padrão é `system`: o Babel consulta o idioma do usuário no sistema operacional,
reconhece variantes como `pt-BR`, `pt-PT` e `en-US`, e usa inglês quando não há
tradução para o idioma detectado ou a detecção não está disponível. O idioma do
navegador não substitui o idioma do sistema que executa o Babel.

```toml
[interface]
language = "system" # system, en ou pt
```

A escolha é salva imediatamente e pode mudar durante uma sessão, preservando
captura, reprodução, conexões de IA, nome da sessão e arquivos abertos. Ela não
altera os idiomas de fala/tradução, prompts, vozes ou rascunhos de outros ajustes.
Arquivos de configuração antigos, sem a seção `interface`, usam `system`.
Textos fornecidos pelo usuário, nomes de dispositivos, identificadores técnicos,
transcrições e mensagens externas de provedores/sistema não são traduzidos.
A documentação técnica disponível nos links de ajuda permanece em português.

Para adicionar traduções ao projeto, consulte [Internacionalização](localization.md).

## Arquivos e chaves

O Babel carrega `babel.toml` a partir do diretório de execução, ou o caminho passado
com `--config`. Sem arquivo, o painel abre com valores padrão; salvar cria o
arquivo. `init` recusa sobrescrever um arquivo existente. Campos desconhecidos,
valores fora dos limites e combinações de capacidades incompatíveis são erros.
A escrita usa arquivo temporário e substituição atômica. Em Unix, os arquivos de
configuração, transcrição e gravação criados têm permissão 0600.

Há quatro perfis persistentes: `providers.gemini`, `providers.openai`,
`providers.elevenlabs` e `providers.local`. Cada faixa seleciona seu tradutor
em `microphone.provider` ou `speaker.provider`. Assim, por exemplo, é possível
usar Gemini no microfone e OpenAI na saída. ElevenLabs é um **sintetizador**, não
aparece como tradutor de speech-to-speech nesta aplicação.

Cada perfil de nuvem tem um `api_key_env`. Os padrões são `GEMINI_API_KEY`,
`OPENAI_API_KEY` e `ELEVENLABS_API_KEY`. Há duas formas de fornecer a chave:

- **Painel, chave desta execução:** a chave fica em um armazenamento de memória
  separado, é apagada ao substituir/remover e não é devolvida pelas APIs do painel.
  Ela tem precedência sobre a variável de ambiente com o mesmo nome. Reiniciar o
  Babel exige informá-la novamente.
- **Variável de ambiente:** defina antes de abrir o Babel. Linux/macOS:
  `export GEMINI_API_KEY='sua-chave'`. PowerShell:
  `$env:GEMINI_API_KEY='sua-chave'`. O painel mostra apenas presença/ausência.

Remover a chave temporária faz o programa voltar à variável de ambiente, se ela
existir. Não escreva chaves em prompts, endpoints, nomes de voz ou arquivos de
configuração. Credenciais já utilizadas em uma sessão ativa são aplicadas de novo
na próxima conexão; pare/reinicie o fluxo quando mudar a conta. `doctor` verifica
presença, não validade remota, créditos ou acesso a modelos.

O servidor do painel escuta somente `127.0.0.1`. A URL inicial contém uma
capacidade aleatória no fragmento; o JavaScript usa autenticação Bearer nos pedidos.
Não exponha esse servidor via proxy/rede sem uma camada de autenticação própria.
O painel verifica Host/Origin e não carrega scripts/fontes de terceiros.

## Encaminhamento e sessão

O Babel tem um caminho de áudio local e uma sessão opcional de processamento.
Enquanto o programa estiver aberto, as rotas configuradas encaminham o microfone
físico para o microfone virtual e a saída virtual para a saída física. Sem uma
sessão ativa, o conteúdo encaminhado é o áudio original, sem chamadas de IA nem
criação de arquivos.

`microphone.enabled` e `speaker.enabled` controlam **somente a tradução** de cada
direção. Eles não desligam captura, reprodução, transcrição ou gravação. Durante
uma sessão, uma direção com tradução desligada continua passando o áudio original.
As opções `transcription.*` e `recording.*` escolhem suas próprias fontes.

**Iniciar sessão** ativa os recursos selecionados. **Encerrar sessão** finaliza
processamento e arquivos e retorna ao encaminhamento original. Para interromper
também esse encaminhamento e liberar a captura, use **Sair do Babel**.
Sem nenhum recurso escolhido, não é necessário iniciar sessão; o botão fica
indisponível e o backend também rejeita esse início.

| Escolhas da sessão | Áudio ouvido nos destinos | Arquivos |
|---|---|---|
| Só gravação | Original em ambas as direções | Um WAV dos originais selecionados. |
| Só transcrição | Original em ambas as direções | Um TXT dos originais selecionados. |
| Transcrição e gravação, sem tradução | Original em ambas as direções | Um TXT e um WAV. |
| Tradução em uma ou ambas as direções | Traduzido nas direções habilitadas; original nas demais | TXT/WAV apenas se habilitados separadamente. |

O painel mostra **Áudio original** quando apenas o encaminhamento está ativo e
**Sessão ativa** durante processamento ou escrita. Um erro de roteamento é
exibido mesmo sem sessão. Configure os dois extremos de cada rota que deseja
utilizar; uma seleção de dispositivo não muda a saída padrão global do sistema.

## Tradutores e vozes são escolhas diferentes

O `provider` da faixa escolhe o tradutor. O `voice.engine` escolhe a origem da voz
final:

| Engine de voz | Comportamento |
|---|---|
| `native` | Usa o áudio do tradutor. É o caminho com menos etapas. |
| `gemini` | Recebe texto traduzido em memória e sintetiza com Gemini TTS. |
| `elevenlabs` | Recebe texto traduzido em memória e sintetiza com ElevenLabs. |

`voice.voice_id` é o identificador da voz na biblioteca do sintetizador. Uma voz
Gemini não é intercambiável com um ID ElevenLabs. No modo nativo conversacional,
este campo pode conter uma voz pronta daquele modelo. Nos modelos dedicados a
tradução, deixe-o vazio: essas APIs não aceitam seleção nativa de voz fixa.

`voice.style` é um prompt de estilo para Gemini TTS. Não é um prompt de tradução.
Ele é rejeitado para o endpoint ElevenLabs implementado e para o modo nativo,
em vez de ser ignorado. `voice.chunk_ms` limita a espera a partir do primeiro
fragmento de texto pendente antes de enviar um trecho incompleto ao TTS, de 100 a 2000 ms. Frases
com pontuação podem ser enviadas antes; trechos longos são divididos por palavras.
Um valor pequeno pode fragmentar a prosódia e aumentar número/custo de requests.

O tradutor Live continua gerando áudio quando se utiliza uma voz TTS. Esse áudio
é descartado pelo adaptador; a sua transcrição de saída é usada apenas em memória
para a síntese. Portanto esse modo pode cobrar **tradução Live e TTS** e aumenta
a latência. Somente os textos originais solicitados pelo usuário são gravados.
No pipeline local, o Piper é dispensado quando um TTS externo fornece a voz final.

Para criar uma voz, abra a biblioteca, escolha Gemini ou ElevenLabs e use design
ou clonagem. Leia [o guia específico](voices.md) antes de preparar os arquivos.
O Babel aceita múltiplos perfis retornados pelo serviço e IDs existentes. Ele não
executa cadastramento oculto de participantes durante a captura.

Com a tradução desligada e a transcrição ligada para uma origem, o perfil da
mesma faixa fornece somente reconhecimento de fala (ASR). O modelo independente
usa `transcription_model`; vazio seleciona `gemini-3.5-transcribe-live` no Gemini
ou `gpt-live-transcribe` no OpenAI. Isso não executa
tradução ou síntese escondidas. No perfil local, somente Whisper é necessário
nesse caminho: Ollama, Piper e o modelo de tradução podem ficar sem configuração.
Gravação e passagem original, sem transcrição nem
tradução, não usam os provedores. Consulte os guias de cada adaptador para os
modelos de reconhecimento compatíveis; loopback não fornece transcrição.
O idioma de origem continua disponível com a tradução desligada; ele pode ser
usado pelo reconhecedor, em vez de herdar detecção automática de um modelo de
tradução que não está em execução.

No reconhecimento independente, o TXT recebe somente resultados finais; parciais
não são repetidas no arquivo. Encerrar a sessão cancela o reconhecimento pendente,
portanto a última frase ainda em processamento pode não aparecer. Aguarde uma
pausa curta e a chegada do resultado final antes de encerrar quando esse trecho
for importante. O writer esvazia os resultados finais já recebidos; não há
garantia de obter a fala ainda não finalizada pelo serviço. A gravação WAV é um
caminho separado e guarda a captura selecionada, independentemente desse resultado.

## Idiomas e prompts

Os idiomas usam códigos BCP-47, como `pt-BR`, `en-US` e `es-ES`. O adaptador pode
normalizar para o código que a API exige. A lista exata de idiomas depende do
modelo; um campo de texto válido não comprova suporte da conta/API.

Gemini Live Translate e OpenAI Realtime Translate detectam a língua de origem.
`source_language` permanece configurável para outros modelos, mas não é um hint
nesses modos dedicados. `target_language` determina o idioma final.

`prompt` contém preferências de tradução por faixa. Está disponível para modelos
conversacionais e para o pipeline local. Os modelos dedicados não aceitam prompts;
o Babel rejeita essa combinação. Um prompt orienta o comportamento, mas não é
uma garantia de fidelidade terminológica ou de ausência de erros do modelo.

Os modelos de voz conversacionais recebem instruções para traduzir perguntas e
comandos, e não executá-los. O aplicativo não concede ferramentas ou comandos de
sistema ao modelo. Texto falado é conteúdo da tradução.

## Dispositivos e qualidade

Escolha dispositivos **explícitos**. Usar aliases de dispositivo padrão poderia
criar realimentação quando a aplicação da chamada passa a usar a saída virtual.
Pela bandeja, é possível trocar o **microfone físico** e a **saída física** durante
a tradução. Essa troca preserva as conexões dos provedores e os arquivos da sessão;
pode causar uma lacuna no áudio. Os demais ajustes do painel, como perfis, idiomas,
vozes, cabos virtuais e opções de gravação, exigem encerrar a sessão. O caminho
original continua ativo durante a configuração. IDs
CoreAudio/WASAPI incluem posição e nome; depois de conectar/desconectar hardware,
atualize a lista e selecione novamente caso o sistema reordene os dispositivos.

`gain` regula o volume do **áudio traduzido** da faixa, entre 0 e 4. O limite de
PCM16 é aplicado por saturação para evitar overflow numérico; valores altos podem
causar clipping audível.

Os perfis de qualidade são ajustes de transporte/VAD, não um controle fictício de
fidelidade do modelo de nuvem:

| Perfil | Quadro local nos modelos não dedicados | Silêncio VAD conversacional |
|---|---:|---:|
| `low_latency` | 10 ms | 200 ms |
| `balanced` | 20 ms | 400 ms |
| `high_quality` | 40 ms | 700 ms |

Nos modelos dedicados, o Babel envia quadros de 100 ms. A reprodução interna usa
PCM16 mono a 24 kHz em quadros de até 20 ms. O servidor Linux ou o resampler Rust
adapta a taxa física. A captura da IA é mono a 16 kHz; o adaptador OpenAI a converte
para 24 kHz. Isso não converte o sistema em áudio de música estéreo hi-fi: o foco
é fala traduzida.

A passagem original usa PCM16 mono a 48 kHz, em quadros de 10 ms, sem a conversão
para fala traduzida. Em uma sessão com tradução desligada, a reprodução original
também permanece a 48 kHz; cópias destinadas ao WAV ou ASR são convertidas para
16 kHz. Os dispositivos virtuais permanecem os mesmos ao iniciar/encerrar uma
sessão, mas reabrir os streams pode produzir um breve intervalo. O Babel precisa
continuar em execução para encaminhar áudio; o início automático é opcional.

- `capture_queue_ms`: 100–1000 ms, padrão 200. Limita filas de captura e envio.
- `max_capture_age_ms`: 100–1000 ms, padrão 200. Quadros locais velhos são descartados.
- `playback_queue_ms`: 100–5000 ms, padrão 2000. É um teto, não atraso deliberado.
- `device_latency_ms`: 5–200 ms, padrão 30. Solicitação de latência ao backend;
  drivers e o sistema podem adotar outro buffer. CPAL usa a configuração nativa
  suportada, portanto esse número não garante buffer físico exato.

Se o tradutor produzir mais áudio do que é possível reproduzir, a fila não cresce
indefinidamente: o fluxo para com erro. Aumentar filas absorve rajadas, mas pode
aumentar o atraso máximo. Repetidos descartes indicam rede, dispositivo, fila ou
hardware insuficientes. Na opção local, o modelo de STT/LLM/TTS precisa processar
mais rápido que a fala para sustentar uma sessão longa.

## Arquivos da sessão

Antes de iniciar, o campo **Nome da sessão** permite um título opcional de até
100 caracteres, como “Reunião com a equipe”. Esse nome vale apenas para a nova
execução; não altera os perfis dos provedores. Vazio gera um título com data/hora
UTC. O painel mantém o nome da sessão após a parada, até outra ser iniciada.
Pela CLI: `babel run --session "Reunião com a equipe"`. Iniciar pela bandeja usa
um nome automático; abra o painel para informar um nome próprio.

O título completo aparece no cabeçalho da transcrição, mesmo com timestamps
desligados. `files.name_pattern` define uma única base de nome para o TXT e o WAV.
O padrão é `{date}-{time}-{session}-{id}`. Os marcadores disponíveis são:

| Marcador | Conteúdo |
|---|---|
| `{date}` | Data de início em UTC, no formato `AAAAMMDD`. |
| `{time}` | Horário de início em UTC, no formato `HHMMSS`. |
| `{session}` | Versão segura do título, em minúsculas, com separadores convertidos em hífens e limite de 64 bytes UTF-8. |
| `{id}` | Identificador aleatório da sessão; obrigatório para diferenciar execuções. |

O padrão aceita de 1 a 128 bytes. Marcadores desconhecidos, caracteres de controle,
separadores de caminho e caracteres reservados são rejeitados. A base resultante
também é validada e limitada a 240 bytes; nomes reservados do Windows e bases
terminadas em ponto ou espaço são rejeitados nas duas saídas. Informe somente a
base: `.txt` e `.wav`
são acrescentados pelo programa. Repetir o título mantém nomes distintos; uma
colisão de arquivo causa erro, nunca sobrescrita. Por exemplo:

```text
transcripts/20260929-150000-reunião-com-a-equipe-1a2b3c4d.txt
recordings/20260929-150000-reunião-com-a-equipe-1a2b3c4d.wav
```

Transcrição e gravação são independentes: cada uma tem `enabled`, `microphone`,
`speaker` e `directory`. Um recurso habilitado precisa selecionar pelo menos uma
origem com captura configurada; essa origem não precisa ter tradução habilitada.
Selecionar nenhuma fonte para um recurso ativo impede o início. As pastas podem
ser relativas a `files.base_path` ou absolutas e podem ser diferentes entre
TXT e WAV. Os arquivos ficam na máquina que executa
o Babel, não na pasta de downloads do navegador. As duas opções vêm desligadas.

```toml
[files]
# Escolha um caminho absoluto adequado ao seu sistema:
# base_path = '/home/ana/Babel'
# base_path = '/Users/ana/Babel'
# base_path = 'C:\Users\Ana\Babel'
name_pattern = "{date}-{time}-{session}-{id}"

[transcription]
enabled = true
microphone = true
speaker = true
timestamps = true
directory = "transcripts"

[recording]
enabled = false
microphone = true
speaker = true
directory = "recordings"
```

`babel init` preenche `base_path` com a pasta `Babel` dentro da pasta pessoal.
Ao criar um TOML manualmente, defina esse campo explicitamente: um arquivo
existente sem `base_path` é tratado como legado e migrado para a pasta do próprio
arquivo de configuração, conforme as regras abaixo.

### Pasta base e destinos

Configure **Ajustes → Arquivos da sessão → Pasta base** no painel. O campo
fica disponível mesmo com gravação e transcrição desligadas. A prévia mostra
os destinos completos calculados pelo sistema que executa o Babel, incluindo
as alterações ainda não salvas. Clique em **Salvar ajustes** antes da próxima
sessão. Alterações de pastas não são aplicadas a uma sessão em andamento e não
movem nem apagam arquivos existentes.

- `files.base_path` é a base comum e precisa ser **um caminho absoluto**.
  Configurações novas usam a pasta `Babel` dentro da pasta pessoal do usuário
  (`HOME` no Linux/macOS e `USERPROFILE` no Windows). O destino não depende da
  pasta em que o terminal, o atalho ou o início automático abre o programa.
- O painel e a API rejeitam bases relativas, incluindo `"."`. Não existe fallback
  para o diretório de execução. Se a pasta pessoal não puder ser determinada,
  configure um caminho absoluto explicitamente.
- `transcription.directory` e `recording.directory`, quando relativos, são
  acrescentados à base. Um caminho absoluto nesses campos ignora a base para
  aquele tipo de arquivo. `"."` nesses campos salva diretamente na pasta base.
- Caminhos seguem as regras do SO do Babel. A leitura da pasta pessoal para o
  padrão não significa expansão de `~`, `$HOME` ou `%USERPROFILE%` nos campos:
  informe o caminho completo. No Windows, a base precisa de unidade completa ou
  UNC; formas ambíguas como `C:pasta` e `\pasta` são rejeitadas.
- A prévia não cria pastas nem testa permissão de escrita. O início da sessão
  cria as pastas necessárias e reporta falhas antes da captura. Links simbólicos
  são seguidos pelo sistema de arquivos; a prévia não os resolve.

Exemplos de base absoluta em TOML (aspas simples preservam as barras do Windows):

| Sistema | Configuração | Destino do WAV com `directory = "recordings"` |
| --- | --- | --- |
| Linux | `base_path = '/home/ana/Babel'` | `/home/ana/Babel/recordings` |
| macOS | `base_path = '/Users/ana/Babel'` | `/Users/ana/Babel/recordings` |
| Windows | `base_path = 'C:\Users\Ana\Babel'` | `C:\Users\Ana\Babel\recordings` |

Por exemplo, para a usuária Ana no Linux, uma configuração nova usa
`/home/ana/Babel`: o TXT fica em `/home/ana/Babel/transcripts` e o WAV em
`/home/ana/Babel/recordings`. Abrir o programa por outro diretório, pelo script ou
pelo início automático não muda esses destinos.

### Migração de configurações antigas

Ao carregar um arquivo existente sem `files.base_path`, o Babel fixa a pasta do
arquivo de configuração como base absoluta. Uma base relativa antiga é resolvida
uma única vez contra essa mesma pasta: por exemplo, `base_path = "sessões"` em
`/home/ana/config/babel.toml` passa a `/home/ana/config/sessões`, e `"."` passa a
`/home/ana/config`. O valor absoluto é persistido por substituição atômica do TOML.
A migração não move nem remove gravações ou transcrições existentes.

Essa regra preserva o destino antigo quando a configuração ficava junto à pasta
usada como base. Se o Babel era iniciado em outro diretório, o destino anterior
não pode ser deduzido do TOML: confira a prévia e informe a pasta absoluta desejada
antes da próxima sessão. Se não for possível salvar a migração, corrija o erro
de escrita do arquivo de configuração e tente novamente.

### Transcrição dos originais

Na página **Transcrição**, habilite o recurso, escolha as origens, os horários
opcionais e a pasta do texto. Use **Pasta base e nomes de arquivos** para alterar
os ajustes compartilhados e **Provedor de reconhecimento** para acessar o perfil
de IA de cada direção em **Tradução e vozes**, sem precisar habilitar tradução.

`transcription.enabled` habilita **um único TXT por sessão**, contendo somente o
texto original das faixas selecionadas. Os rótulos `[microfone]` e `[saída recebida]`
identificam de onde veio cada trecho; não identificam participantes. O texto
traduzido utilizado internamente na síntese de voz não é gravado nesse arquivo.

Os dois adaptadores podem entregar fragmentos com atrasos diferentes. O TXT segue
a ordem em que eles chegam ao Babel; não reordena falas por timestamps, nem
promete reconstruir a cronologia exata de uma conversa sobreposta.

O arquivo é gravado incrementalmente em um worker separado. Erro de escrita
ou fila de transcrição saturada é reportado; não há descarte silencioso de texto.
Os trechos preservam os espaços entre deltas do provedor. O conteúdo é a
transcrição fornecida pelo modelo, sem uma segunda tradução.

Com `timestamps = true`, os marcadores têm semântica explícita:

- `[áudio +início–fims]`: offsets de áudio fornecidos/medidos pelo adaptador.
- `[alinhamento +Xs]`: ponto aproximado fornecido pela API, não limite de palavra.
- `[recebido ...]`: horário UTC de chegada do texto ao Babel; inclui atraso da IA.

Não se deve interpretar horário de recebimento como instante exato da fala.
Reconexões são marcadas e podem reiniciar os offsets do provedor. Com timestamps
desativados, esses marcadores de fala são omitidos; o cabeçalho ainda informa a
sessão. IDs de falante reais, se recebidos, ficam no arquivo. Ausência de ID não
é preenchida com nomes inventados. A identificação automática persistente de
pessoas/clones dentro de uma chamada misturada é uma limitação atual.

### Gravação dos originais

Na página **Gravação**, habilite o recurso e escolha as origens e a pasta do áudio.
O atalho **Pasta base e nomes de arquivos** abre os ajustes compartilhados com a
transcrição em **Ajustes → Arquivos da sessão**.

`recording.enabled` habilita **um único WAV PCM16 little-endian, mono a 16 kHz**.
Ele recebe a captura original do microfone físico e a captura original da saída
virtual, conforme as seleções `microphone` e `speaker`. As origens são misturadas
em uma faixa única. O ponto de captura fica antes da tradução e do ganho do áudio
traduzido: vozes da IA e alterações desse ganho não entram na gravação.

É possível gravar sem transcrever, transcrever sem gravar ou ativar ambos.
Nenhuma dessas opções depende de `microphone.enabled` ou `speaker.enabled`: esses
campos habilitam apenas a tradução. Uma sessão de gravação sem transcrição nem
tradução não requer chave de IA.
A troca de dispositivo físico pela bandeja mantém o mesmo arquivo da sessão,
com possível lacuna durante a troca. Para formato, mistura, falhas de disco e
limites, consulte [gravação dos originais](recording.md).

## Bandeja e encerramento

`serve` abre um serviço de bandeja no Linux via StatusNotifier/KSNI, sem GTK.
O desktop precisa suportar StatusNotifier; algumas instalações GNOME requerem
suporte AppIndicator habilitado. Sem esse suporte, use o link impresso ou
`serve --no-tray`. Windows e macOS usam menus nativos e um event loop próprio.

**Iniciar sessão** usa os recursos da configuração salva; **encerrar sessão**
cancela IA, finaliza os arquivos e retorna ao áudio original; **configurações**
abre o painel no navegador; **sair** encerra também captura, encaminhamento e
servidor. Fechar a aba do painel mantém o áudio funcionando.

Os submenus **Microfone físico** e **Saída física** mostram os dispositivos reais
e marcam a seleção atual. O primeiro altera a captura da faixa de microfone;
o segundo altera onde a faixa de saída reproduz a tradução. Os cabos virtuais
permanecem os mesmos. A troca funciona também durante uma sessão ativa, preservando
o provedor, a transcrição e a gravação em andamento. **Atualizar dispositivos**
refaz a enumeração depois de conectar ou desconectar um headset. Uma seleção
inválida não faz fallback silencioso para outro dispositivo.

Se o dispositivo falhar ou for removido, o painel informa o erro. Selecione outro
dispositivo físico pela bandeja para recuperar o áudio na mesma sessão. O trecho
indisponível pode produzir uma lacuna; áudio antigo não fica acumulado para ser
reproduzido ou reenviado depois. Mudar idiomas, modelos, vozes e demais opções
continua exigindo encerrar a sessão de processamento.

Essas escolhas são salvas no mesmo TOML do painel. O painel acompanha mudanças
externas; se houver ajustes locais ainda não salvos, pede recarregamento antes
de salvar ou iniciar. As operações verificam a revisão da configuração no servidor,
impedindo que uma aba antiga sobrescreva uma seleção feita pela bandeja.

No modo `run`, Ctrl+C encerra. Nenhum desses caminhos remove os módulos virtuais.
A ação explícita `uninstall` faz a limpeza dos módulos Linux pertencentes ao Babel.

## Início opcional com o login

Em **Inicialização**, marque **Iniciar Babel ao entrar no sistema** e aplique.
O registro é do usuário atual e abre a bandeja/painel. O áudio original é capturado
e encaminhado pelos dispositivos configurados; os recursos de tradução, transcrição
e gravação aguardam uma sessão. Desmarcar e aplicar remove o registro. A simples abertura do painel não
altera a configuração de login. Uma instância nova gera outra URL/token local;
abra o painel pelo menu **Abrir configurações** da bandeja.

O registro não armazena chaves de API. Chaves temporárias digitadas na execução
anterior precisam ser informadas novamente, a menos que você tenha configurado
as variáveis no ambiente da sessão do usuário. Leia [inicialização por sistema](autostart.md)
para os arquivos utilizados, portabilidade dos caminhos e requisitos do desktop.
