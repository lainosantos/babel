# Transcrição dos áudios originais (STT)

O menu **Transcrição** tem seus próprios providers, modelos, credenciais e idiomas.
O reconhecedor STT não depende do tradutor speech-to-speech (STS) nem do
sintetizador de vozes (TTS). É possível, por exemplo, traduzir com Gemini e
transcrever com Deepgram, ou usar somente whisper.cpp para transcrever sem
traduzir. O microfone e a saída recebida podem usar reconhecedores diferentes.

## Configurar no painel

1. Abra **Transcrição** e habilite a transcrição original.
2. Selecione as origens: **Microfone original**, **Saída original**, ou ambas.
3. Em cada origem, escolha o provider STT e o idioma original. `auto` depende
   das capacidades do modelo; um idioma explícito pode melhorar o reconhecimento.
4. Na seção **Providers de transcrição**, configure cada perfil selecionado:
   modelo, referência da chave de API, endpoint e opções disponíveis.
5. Para um serviço em nuvem, adicione a chave no próprio perfil STT ou forneça a
   variável de ambiente correspondente ao iniciar o Babel. Para whisper.cpp,
   mantenha **Integrado ao Babel**. Whisper Base Q5_1 é o padrão compacto;
   Tiny Q5_1 e Small Q5_1 também estão disponíveis. Salvar prepara os arquivos
   automaticamente. Um servidor externo é opcional.
6. Configure a pasta base **absoluta**, a pasta de destino, o padrão do nome e,
   opcionalmente, os tempos dos segmentos. Salve e inicie a sessão com um nome.

Os perfis são compartilhados pelas duas origens dentro da seção STT. Para usar
idiomas distintos, configure cada origem; para usar contas distintas do mesmo
provider por origem, seriam necessários perfis adicionais, ainda não disponíveis.
As configurações de sessão são aplicadas ao iniciar; a troca do dispositivo
físico durante a sessão continua disponível no roteamento e na bandeja.

As chaves inseridas no painel ficam somente na memória do processo e são apagadas
ao sair. O arquivo TOML armazena a **referência** (`api_key_env`), nunca a chave.
Por padrão, STT e tradução podem apontar para a mesma variável da conta. Para
separar credenciais, use nomes diferentes, por exemplo `GEMINI_STT_API_KEY` e
`GEMINI_TRANSLATION_API_KEY`. Alterar a referência do perfil STT não altera o
perfil de tradução ou voz.

## Transcrever o histórico recente

Depois de escolher as origens e os reconhecedores, abra **Opções avançadas de
início** junto de **Iniciar sessão**. Marque **Incluir histórico recente** e
informe a duração em minutos. Sem essa escolha, a transcrição começa no áudio
atual. Esse também é o padrão ao iniciar pela bandeja, por `babel run` na CLI
ou pela API sem `history_seconds` positivo. A retenção habilitada não inclui
histórico automaticamente. A escolha é exclusiva desse início e volta a ficar desmarcada após uma
sessão iniciada com sucesso.

O histórico usa os reconhecedores, idiomas e origens atualmente selecionados
em **Transcrição**, mesmo se forem diferentes dos que estavam configurados
quando o áudio foi capturado. Somente áudio original é reconhecido. Se a
transcrição estiver desligada, incluir histórico para uma gravação não ativa
STT nem envia esse áudio a um provider. Com STT em nuvem habilitado, incluir
histórico envia o trecho solicitado ao serviço escolhido ao iniciar a sessão.

Os resultados do histórico precedem os resultados ao vivo no TXT. O painel
indica enquanto a transcrição do histórico está pendente; o roteamento, a
reprodução e a tradução continuam com áudio ao vivo. Reconhecer vários minutos
pode levar tempo e consumir a cota do provider. Encerrar antes de concluir pode
interromper resultados pendentes, como ocorre com a transcrição ao vivo.

A capacidade padrão é dez minutos em memória, ajustável em **Ajustes →
Histórico de áudio recente**. O painel mostra o áudio disponível por origem;
se houver menos que o solicitado, inclui somente o trecho disponível. O
histórico só se forma enquanto há captura pelo roteamento, não é persistido
antes da inclusão explícita e desaparece ao fechar o Babel. Os mesmos controles
estão disponíveis no Linux, macOS e Windows. Consulte [a retenção e a inclusão
na gravação](recording.md#incluir-áudio-anterior-ao-início).

O reconhecimento do histórico usa uma conexão STT separada da transcrição ao
vivo. Continuam valendo as cotas, custos e limites de sessão do provider. Se
uma conexão expirar ou a sessão for encerrada antes da conclusão, o Babel
informa que o histórico ficou incompleto; não apresenta esse resultado como
uma recuperação integral. Identificadores de falantes não são associados
automaticamente entre as conexões histórica e ao vivo.

## Providers implementados

| Provider STT | Transporte e modelo | Falantes | Tempos gravados | Requisitos |
| --- | --- | --- | --- | --- |
| Gemini Live Transcribe | WebSocket; `gemini-3.5-transcribe-live` | Sem diarização confirmada no streaming atual | Recebimento; metadados reais quando presentes | Chave Google com acesso ao modelo |
| OpenAI Realtime Transcription | WebSocket; `gpt-live-transcribe` por padrão | Sem diarização neste adaptador | Recebimento | Chave OpenAI com acesso ao modelo |
| Deepgram Listen | WebSocket v1; `nova-3` por padrão, também Nova-2 | Opcional; IDs enviados pela API | Intervalos dos segmentos, derivados das palavras retornadas | Chave Deepgram e modelo/idioma compatíveis |
| whisper.cpp | Motor integrado; Tiny/Base/Small multilíngues, com variantes compactas Q5_1. Servidor HTTP externo opcional | Sem diarização neste adaptador | Limites dos segmentos enviados ao reconhecedor | Instalador com runtimes; internet apenas para preparar pesos ausentes |

Suporte implementado não garante disponibilidade do modelo para toda conta,
região ou idioma. Testes automatizados usam servidores simulados locais e não
medem a qualidade das APIs com áudio real. A configuração e os adaptadores Rust
são comuns ao Linux, macOS e Windows; a instalação do servidor whisper.cpp é
específica do sistema.

### Gemini Live Transcribe

Selecione **Gemini** na origem e configure o perfil em Transcrição. O endpoint
oficial é fixo; o modelo STT é `gemini-3.5-transcribe-live`, separado do modelo de
tradução. A entrada é PCM16 mono a 16 kHz. A sessão solicita saída de texto e
preserva a fala original, sem idioma de destino, prompt de tradução ou voz TTS.

O Babel salva os segmentos finais de `inputTranscription`; hipóteses
especulativas de `interimInputTranscription` não são gravadas como texto final.
`auto` omite a restrição de idioma; um código explícito é enviado em
`languageCodes`. O streaming atual não oferece diarização nem timestamps por
palavra garantidos. Os recursos da API de **arquivos** não devem ser confundidos
com os do Live Transcribe. A duração máxima documentada da sessão Live é de dez
minutos; reconexões podem produzir uma lacuna e reiniciar o relógio do provider.

Fontes: [Live Transcribe](https://ai.google.dev/gemini-api/docs/live-api/live-transcribe)
e [capacidades do modelo](https://ai.google.dev/gemini-api/docs/models/gemini-3.5-transcribe).

### OpenAI Realtime Transcription

O perfil STT usa `gpt-live-transcribe` por padrão. Famílias adicionais aceitas pelo
adaptador são `gpt-transcribe` e `gpt-realtime-whisper`, incluindo snapshots
datados válidos. Modelos de conversa ou tradução de áudio não são modelos STT.

Deixe o endpoint vazio para usar o endereço oficial de Realtime transcription.
Um endereço explícito deve falar o mesmo protocolo WebSocket; `/translations`
é recusado. O Babel abre uma sessão `type: transcription`, converte o PCM de
16 para 24 kHz e envia segmentos com commit por VAD local (400 ms de silêncio).
Salva apenas os eventos finais, respeitando a ordem dos segmentos enviados.

O idioma explícito configura o reconhecimento; `auto` deixa o modelo decidir
quando suportado. `gpt-realtime-whisper` exige idioma explícito. Este adaptador
não solicita diarização nem gera timestamps por palavra; a opção de tempos do
Babel registra o recebimento quando não há alinhamento fornecido pelo serviço.
Campos como prompt de tradução, voz e idioma de destino não são enviados.

Fonte: [Realtime transcription](https://developers.openai.com/api/docs/guides/realtime-transcription).

### Deepgram

O endpoint padrão é `wss://api.deepgram.com/v1/listen`, com modelo `nova-3` e chave
referenciada por `DEEPGRAM_API_KEY`. O adaptador aceita modelos da família
Nova-2/Nova-3 compatíveis com **Listen v1**; Flux usa outro protocolo e não está
implementado aqui. A entrada é PCM16 mono a 16 kHz enviada em binário.

O idioma `auto` usa `language=multi` nos modelos gerais compatíveis. Isso permite
reconhecimento multilíngue dentro dos idiomas suportados pelo modelo, não detecção
universal de qualquer idioma. Para modelos especializados, escolha um idioma
explícito compatível. A opção de pontuação envia `punctuate` ao serviço.

Com **Identificar falantes** habilitado, o Babel envia `diarize_model=v1`, agrupa
palavras consecutivas pelo ID de falante retornado e grava os tempos reais de
início/fim desses grupos. Com a opção desligada, não pede diarização. IDs numéricos como
`0` identificam agrupamentos da API, não pessoas verificadas; podem mudar
após uma reconexão ou entre as duas origens. Esta opção não clona vozes e não
altera a voz da tradução.

Apenas resultados `is_final` são persistidos. `speech_final` encerra o turno sem
duplicar texto. Durante o silêncio, o cliente envia keepalive a cada três
segundos. Falhas transitórias usam o orçamento de reconexões configurado;
autenticação recusada não fica em repetição infinita.

Fontes: [Listen v1](https://developers.deepgram.com/reference/speech-to-text/listen-streaming),
[diarização](https://developers.deepgram.com/docs/diarization),
[multilíngue](https://developers.deepgram.com/docs/multilingual-code-switching)
e [keepalive](https://developers.deepgram.com/docs/audio-keep-alive).

### whisper.cpp local

Selecione `whisper.cpp` no reconhecimento da origem e mantenha **Integrado ao
Babel** no perfil. Ao salvar, o Babel baixa e verifica os pesos multilíngues
escolhidos, quando ainda não estiverem no cache. O motor só ocupa RAM quando
uma sessão com transcrição habilitada precisa dele. Não é
necessário instalar Python, CMake, Ollama ou iniciar um servidor separado. O
painel mostra preparação, progresso de download, disponibilidade e falhas.

Escolha `tiny-q5_1` (32,2 MB), `base-q5_1` (padrão, 59,7 MB) ou `small-q5_1`
(190,1 MB). São tamanhos dos pesos, não da RAM total. As opções originais
`tiny`, `base` e `small` continuam válidas, inclusive em configurações existentes.
Tiny prioriza baixo consumo e pode perder precisão; Small usa mais recursos.
O resultado depende do hardware, do idioma, do sotaque e do ruído.
A pasta de modelos, threads de CPU e prazo de liberação de memória ficam em
**Ajustes → Modelos locais**. O padrão usa até dois threads; após a última
sessão liberar o motor, ele é encerrado depois de 60 segundos por padrão.
Os pesos permanecem em disco. O prazo `idle_unload_secs` aceita 1–3600 segundos.
Depois da preparação, esse reconhecimento funciona sem internet. O motor usa
uma porta local dinâmica: nenhuma porta padrão é presumida ou salva no perfil.

```toml
[transcription.microphone_recognition]
provider = "whisper"
language = "pt-BR"

[transcription.providers.whisper]
endpoint = "auto"
model = "base-q5_1"
api_key_env = ""
segment_ms = 2000
silence_ms = 300
vad_threshold = 0.01
request_timeout_secs = 30

[local_runtime]
directory = "" # Cache da conta, ou caminho absoluto escolhido pelo usuário.
threads = 2
idle_unload_secs = 60
```

Esse perfil reconhece somente o áudio original. Não chama o modelo tradutor,
Piper ou um serviço de voz. `auto` no **idioma** pede detecção ao Whisper e códigos
como `pt-BR` são reduzidos a `pt`; `translate` é sempre falso. `endpoint = "auto"`
seleciona o processo gerenciado, não o idioma.

O áudio é segmentado por VAD de energia, com duração máxima, silêncio, limiar RMS
e timeout configuráveis. Segmentos menores reduzem espera, mas podem reduzir
contexto. Modelos lentos aumentam latência; não se trata de streaming neural
contínuo. Os tempos correspondem aos limites do áudio enviado, sem alinhamento
de palavras nem identificação de falantes.

**Servidor externo (avançado)** continua disponível para uma instalação própria.
Informe a URL completa de inferência com a porta real. Nesse modo, o modelo é
carregado pelo seu servidor, e a escolha de modelo Whisper do Babel não o altera.
O prazo de liberação de memória do Babel não encerra esse servidor externo.
Autenticação Bearer é opcional somente no modo externo: informe uma referência
de chave quando o servidor/proxy exigir. HTTP sem TLS é aceito apenas em
loopback; endereços remotos exigem HTTPS. Redirecionamentos e URLs com
credenciais, query ou fragmento não são aceitos.

Leia [Modelos locais integrados](local-inference.md) para armazenamento,
preparação, instalação e limites. Referência do protocolo externo:
[servidor whisper.cpp](https://github.com/ggml-org/whisper.cpp/tree/master/examples/server).

## Exemplo TOML independente

Este trecho usa Gemini para reconhecer o microfone e Deepgram para reconhecer
as falas recebidas. Os tradutores continuam configurados separadamente em
`microphone`, `speaker` e `providers`. Omita os perfis não utilizados ou mantenha
seus defaults; nenhuma chave de provider inativo é exigida.

```toml
[transcription]
enabled = true
microphone = true
speaker = true
timestamps = true
directory = "transcripts"

[transcription.microphone_recognition]
provider = "gemini"
language = "pt-BR"

[transcription.speaker_recognition]
provider = "deepgram"
language = "en"

[transcription.providers.gemini]
api_key_env = "GEMINI_STT_API_KEY"
model = "gemini-3.5-transcribe-live"
connect_timeout_secs = 15
max_reconnect_attempts = 5

[transcription.providers.deepgram]
api_key_env = "DEEPGRAM_STT_API_KEY"
endpoint = "wss://api.deepgram.com/v1/listen"
model = "nova-3"
diarize = true
punctuate = true
connect_timeout_secs = 15
max_reconnect_attempts = 3
```

Configure também `files.base_path` com um caminho absoluto válido no sistema,
por exemplo `/home/usuario/Babel`, `/Users/usuario/Babel` ou `C:\Users\usuario\Babel`.
Consulte [configuração](configuration.md) para o padrão do nome e a resolução de
pastas; o destino não depende de onde o aplicativo foi iniciado.
O caminho não precisa existir previamente. Ao iniciar uma sessão com transcrição,
o Babel cria recursivamente a pasta de destino e todos os diretórios pais que
faltarem, inclusive a base, no Linux, macOS e Windows. Isso também funciona com
gravação de áudio desligada. Só há erro de acesso ao destino quando não é possível
criar a pasta ou abrir o arquivo; a simples inexistência da pasta não impede a sessão.

## Arquivo, roteamento e limites

- Uma sessão produz **um TXT** com as origens selecionadas, identificadas como
  microfone ou saída recebida, e somente no idioma original. Com ambas ativas,
  resultados entram no arquivo conforme são recebidos; atrasos distintos entre
  providers não garantem uma ordenação global exata das falas.
- O reconhecedor recebe o áudio original, antes da tradução e da voz gerada.
  Mesmo quando STS fornece texto auxiliar, esse texto não substitui nem duplica
  a transcrição do STT selecionado.
- As marcações temporais são opcionais. Quando há metadados, preservam offsets
  da sessão de áudio do provider; caso contrário indicam recebimento. Não são
  garantia de timestamps por palavra nem de uma linha do tempo única após
  reconexões. Lacunas são assinaladas no TXT.
- Transcrição, tradução e gravação são habilitadas separadamente. Gravar somente
  áudio não abre STT. Se ambas as direções forem selecionadas para transcrição,
  haverá duas conexões/requisições independentes, mesmo usando o mesmo provider.
  Tradução e STT em nuvem simultâneos também podem ter cobranças separadas.
- O microfone é processado enquanto Babel é o microfone padrão do sistema ou
  um aplicativo usa seu microfone virtual. A saída exige um aplicativo enviando
  áudio ao Babel. Ao desativar uma rota, o Babel cancela seus processadores e
  descarta áudio antigo das filas.
  A ativação de comandos por voz continua restrita ao microfone e usa sua própria
  configuração, sem depender do STT de arquivo.
- Filas são limitadas e o roteamento não aguarda uma chamada STT. Congestionamento
  pode descartar frames de processamento para evitar atraso ilimitado. Erros
  fatais de um processador encerram a sessão e retornam ao roteamento original;
  a independência das opções não significa recuperação isolada de toda falha.

## Configurações antigas

Ao carregar um TOML anterior a essa separação, o Babel migra os campos ausentes:
copia o provider e idioma originais de cada rota, converte `local` para
`whisper`, e copia referências de chave e modelos ASR para os novos perfis STT.
Campos STT já definidos são preservados. A migração não ativa funcionalidades
que estavam desligadas. A partir daí, alterações de tradução não alteram STT.

O endpoint oficial OpenAI `/realtime/translations` é convertido para
`/realtime` dentro do novo perfil de reconhecimento. Um endpoint personalizado
de tradução exige configuração STT explícita, evitando adivinhar outro destino.
Endereços Whisper personalizados são preservados. Perfis locais vazios ou
com os defaults antigos reconhecidos migram para preparação integrada;
portas fixas antigas não são reaproveitadas automaticamente. Não existe provider de diagnóstico
`loopback`: para passagem original, basta desligar a tradução.
