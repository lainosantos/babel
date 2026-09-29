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
   informe o endereço real do servidor; o Babel não presume uma porta.
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

## Providers implementados

| Provider STT | Transporte e modelo | Falantes | Tempos gravados | Requisitos |
| --- | --- | --- | --- | --- |
| Gemini Live Transcribe | WebSocket; `gemini-3.5-transcribe-live` | Sem diarização confirmada no streaming atual | Recebimento; metadados reais quando presentes | Chave Google com acesso ao modelo |
| OpenAI Realtime Transcription | WebSocket; `gpt-live-transcribe` por padrão | Sem diarização neste adaptador | Recebimento | Chave OpenAI com acesso ao modelo |
| Deepgram Listen | WebSocket v1; `nova-3` por padrão, também Nova-2 | Opcional; IDs enviados pela API | Intervalos dos segmentos, derivados das palavras retornadas | Chave Deepgram e modelo/idioma compatíveis |
| whisper.cpp | HTTP multipart para `/inference`; modelo carregado no servidor | Sem diarização neste adaptador | Limites dos segmentos enviados ao reconhecedor | Servidor whisper.cpp e modelo multilíngue local |

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

Inicie um servidor whisper.cpp com um modelo multilíngue apropriado, obtenha o
endereço e a porta efetivamente usados e informe a URL completa de inferência no
perfil STT. Nenhuma conexão é tentada em uma porta presumida. Um processo que
tenha escolhido outra porta exige atualizar esse endereço. O campo começa vazio.

Esse perfil usa apenas o servidor de reconhecimento: não chama Ollama, Piper,
Gemini ou um serviço de voz. O modelo é escolhido ao iniciar o servidor
whisper.cpp, não pelo painel do Babel. `auto` pede detecção ao Whisper e códigos
como `pt-BR` são reduzidos ao idioma `pt`. O parâmetro `translate` é sempre falso.

O áudio é segmentado por VAD de energia, com duração máxima, silêncio, limiar RMS
e timeout configuráveis. Segmentos menores reduzem o tempo de espera, mas podem
reduzir o contexto linguístico. Um modelo lento ou um computador sem capacidade
suficiente aumenta a latência; não se trata de streaming neural contínuo. Os
tempos gravados correspondem aos limites do áudio enviado, sem alinhamento de
palavras nem identificação de falantes.

Autenticação Bearer é opcional: informe uma referência de chave apenas se seu
servidor/proxy a exigir. Conexões HTTP sem TLS são aceitas somente em loopback;
endereços remotos exigem HTTPS. Redirecionamentos e URLs com credenciais,
query ou fragmento não são aceitos. Não use um endpoint de tradução como STT.

Referência: [servidor whisper.cpp](https://github.com/ggml-org/whisper.cpp/tree/master/examples/server).

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
- O Babel processa uma rota somente enquanto o dispositivo virtual correspondente
  estiver em uso. Ao desativá-lo, cancela os processadores e descarta áudio antigo.
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
Um endereço Whisper só é migrado se estava escrito no TOML; defaults antigos
implícitos de porta não são recriados. Não existe provider de diagnóstico
`loopback`: para passagem original, basta desligar a tradução.
