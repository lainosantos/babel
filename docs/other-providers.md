# OpenAI, Deepgram e inferência local

Cada rota escolhe seu provider de tradução independentemente. Por exemplo, o microfone pode
usar OpenAI e a saída recebida pode usar a cadeia local. Vozes adicionais e
ressíntese de texto traduzido estão em [voices.md](voices.md); dispositivos
selecionáveis em cada sistema estão em [platforms.md](platforms.md).
A transcrição possui escolhas separadas de provider e idioma por origem e seus
próprios perfis `transcription.providers.*`. Ela recebe o áudio original, mesmo
com tradução simultânea, e não grava o texto de entrada emitido pelo STS.
Consulte [Transcrição: Gemini, OpenAI, Deepgram e Whisper](transcription.md).

## OpenAI: tradução contínua ou modelo conversacional

O perfil OpenAI usa `gpt-realtime-translate` por padrão. É um modelo dedicado à
tradução contínua: recebe áudio enquanto produz fala traduzida e texto. Sua API
é `/v1/realtime/translations`, diferente da API conversacional. Para configurar
instruções livres e voz fixa, escolha `gpt-realtime-2.1`, que opera com detecção
de fim de fala e respostas. Essa distinção vem da
[documentação oficial de tradução](https://developers.openai.com/api/docs/guides/realtime-translation).

No painel, escolha OpenAI na rota, informe os idiomas e configure a credencial
OpenAI da sessão. Também é possível fornecer a variável `OPENAI_API_KEY` ao
processo. Use uma chave de projeto com acesso ao modelo.

Trecho para mesclar ao arquivo TOML existente:

```toml
[providers.openai]
api_key_env = "OPENAI_API_KEY"
model = "gpt-realtime-translate"
endpoint = ""
voice = "marin"
connect_timeout_secs = 15
max_reconnect_attempts = 5

[microphone]
provider = "openai"
source_language = "pt-BR"
target_language = "en"
prompt = ""

[speaker]
provider = "openai"
source_language = "en"
target_language = "pt"
prompt = ""
```

Preserve os campos de dispositivos do seu arquivo: os trechos acima mostram
somente as mudanças de provider e idioma. `endpoint = ""` seleciona a URL
oficial correta para o modelo. Um endpoint explícito permanece exatamente no
host/caminho escolhido, com o modelo acrescentado pelo adaptador. Exige `wss`,
exceto para `ws` no loopback; não aceita credenciais, query ou fragmento na URL.
A chave é enviada ao endpoint configurado no cabeçalho Bearer. Compatibilidade
com um servidor alternativo depende de ele implementar o mesmo protocolo GA;
este campo não transforma APIs arbitrárias em providers compatíveis.

No modelo dedicado, prompts personalizados e seleção nativa de voz não são
suportados pelo contrato da sessão. O campo de voz do perfil serve ao modelo
conversacional. O adaptador não promete clonagem, preservação de identidade
vocal ou separação de participantes no modelo dedicado. Use a camada de
ressíntese para escolher uma voz fixa quando necessário. A sessão dedicada
permite definir idioma de saída e transcrição de entrada conforme o
[contrato de eventos de tradução](https://developers.openai.com/api/reference/resources/realtime/translation-client-events).
O campo de idioma de origem não força uma língua na sessão dedicada; a tradução
parte do áudio recebido. No modelo conversacional, ele integra as instruções.

Para tradução com instruções e voz nativa:

```toml
[providers.openai]
model = "gpt-realtime-2.1"
endpoint = ""
voice = "marin"

[microphone]
provider = "openai"
source_language = "pt-BR"
target_language = "en-US"
prompt = "Preserve os termos técnicos de desenvolvimento de software."
```

O adaptador configura PCM16 mono a 24 kHz nas duas direções da conexão. A
captura interna do Babel chega a 16 kHz e é convertida por filtro sinc; isso
não recupera frequências ausentes do sinal original. A versão conversacional
usa o contrato GA `session.audio.input/output` e não interrompe a tradução
anterior automaticamente quando detecta nova fala. Consulte
[Realtime conversations](https://developers.openai.com/api/docs/guides/realtime-conversations)
e [WebSockets](https://developers.openai.com/api/docs/guides/voice-websockets?voice-api=realtime).

A sessão de tradução não solicita reconhecimento original para gerar o TXT.
Seu texto traduzido é usado em memória quando uma voz TTS externa precisa dele.
Para salvar a fala original, selecione e configure um STT na página
**Transcrição**, independentemente do modelo de tradução OpenAI. Os campos
legados `providers.*.transcription_model` não substituem
`transcription.providers.*.model` depois da migração. Veja o
[guia de configuração e migração da transcrição](transcription.md).

A conexão só libera captura depois da confirmação de configuração. Reconexões
têm tentativas limitadas e descartam áudio antigo: não reproduzem a fila acumulada
nem afirmam restaurar contexto anterior. Parar a rota cancela imediatamente
áudio pendente, podendo cortar a última tradução. Mensagens, áudio e filas têm
limites; erros remotos não expõem a chave nem o conteúdo bruto das respostas.
A disponibilidade real do modelo exige validação com sua própria conta; os
[detalhes oficiais do modelo](https://developers.openai.com/api/docs/models/gpt-realtime-translate)
não garantem acesso para todas as contas.

## OpenAI: transcrição independente

Na página **Transcrição**, escolha OpenAI em uma ou nas duas origens. Use o
perfil `transcription.providers.openai`, cujo modelo padrão é
`gpt-live-transcribe`. O adaptador também aceita as famílias compatíveis
`gpt-transcribe` e `gpt-realtime-whisper`, incluindo snapshots datados validados.
Chave, modelo e endpoint desse perfil não são obtidos do perfil de tradução.
O áudio original pode continuar passando ou ser traduzido por outro provider;
o reconhecedor STT produz somente texto original.

```toml
[transcription.microphone_recognition]
provider = "openai"
language = "pt-BR"

[transcription.providers.openai]
api_key_env = "OPENAI_STT_API_KEY"
model = "gpt-live-transcribe"
endpoint = ""
connect_timeout_secs = 15
max_reconnect_attempts = 5
```

Ative `transcription.enabled` e selecione as origens que deseja guardar.
`transcription.speaker_recognition` configura separadamente o áudio recebido;
os perfis STT podem compartilhar a mesma chave por opção, sem compartilhar suas
configurações com a tradução.

O endpoint vazio seleciona `/realtime?intent=transcription`. Um endpoint explícito
que termina em `/translations` é rejeitado nesse modo. O adaptador usa sessão
`type=transcription` e PCM16 mono a 24 kHz, convertido da captura destinada à IA.
O idioma de origem é encaminhado conforme o contrato do modelo; destino, voz e
prompt de tradução não são enviados. Consulte a
[documentação oficial de Realtime transcription](https://developers.openai.com/api/docs/guides/realtime-transcription).

O Babel detecta pausas localmente com limiar RMS de 0,01 e silêncio de 400 ms
no adaptador STT; também fecha trechos de até dez segundos. Registra somente
resultados finais, associados aos commits por `item_id`, com no máximo 64 itens
pendentes para reordenação. Esse ordenamento por direção não sincroniza as falas
das duas conexões. O modelo padrão não fornece diarização nem tempos por palavra.

Encerrar a sessão cancela reconhecimento pendente, podendo perder a última
frase ainda não finalizada. Aguarde uma pausa curta e o resultado final antes de
encerrar; resultados já recebidos são drenados para o TXT. Para conservar a
captura original independentemente da resposta do reconhecedor, a gravação WAV
pode ser habilitada separadamente.

## Deepgram: transcrição contínua dos originais

Selecione `deepgram` no reconhecimento de cada origem desejada e configure
`transcription.providers.deepgram`. O padrão é Nova-3 no endpoint WebSocket
`wss://api.deepgram.com/v1/listen`; a chave usa `DEEPGRAM_API_KEY` por padrão e
segue no cabeçalho `Authorization: Token …`, nunca na URL.

A entrada é PCM16 mono a 16 kHz. O adaptador registra resultados finais, sem
hipóteses parciais, e não produz tradução ou voz. A opção `diarize` habilita o
diarizador streaming v1 (`diarize_model=v1`); palavras consecutivas com o mesmo
ID de falante são agrupadas, preservando pontuação e tempos fornecidos pela
Deepgram. IDs são rótulos da conexão, não nomes nem identificação persistente
das pessoas. A opção `punctuate` controla pontuação.

`language = "auto"` seleciona o modo `multi` dos modelos gerais Nova-2/Nova-3.
Esse modo cobre o conjunto multilíngue do modelo, não todas as línguas disponíveis
isoladamente. Flux usa outro protocolo e não é aceito por este adaptador.
Reconexões têm orçamento limitado e descartam áudio acumulado; rótulos de falante
e offsets podem reiniciar. Keepalive mantém a conexão durante silêncio sem
inventar áudio ou avançar seus timestamps. Veja a [configuração completa](transcription.md),
o [contrato Listen v1](https://developers.deepgram.com/reference/speech-to-text/listen-streaming),
[diarização](https://developers.deepgram.com/docs/diarization) e
[modo multilíngue](https://developers.deepgram.com/docs/multilingual-code-switching).

## Provider local integrado: Whisper → Qwen → Piper

Escolha **Local** na rota de tradução e mantenha **Integrado ao Babel** nos
componentes de reconhecimento, tradução e voz. Ao salvar, o Babel baixa e
verifica os modelos ausentes, mantendo os arquivos em disco. Os motores do
instalador são carregados quando a sessão usa as funções habilitadas. Isso funciona com
o mesmo fluxo no Linux, macOS e Windows, sem instalar Python, CMake, Ollama ou
Piper separadamente. O painel acompanha preparação e download.

A cadeia reconhece WAV mono PCM16/16 kHz com Whisper, traduz o texto com Qwen
via llama.cpp e sintetiza com Piper. O Babel converte o WAV resultante para
24 kHz e o entrega em frames de 20 ms. A implementação mantém os nomes dos
campos antigos de endpoint para compatibilidade; `ollama_endpoint = "auto"`
inicia llama.cpp, não exige um serviço Ollama.

```toml
[providers.local]
whisper_endpoint = "auto"
whisper_model = "base-q5_1"
ollama_endpoint = "auto"
translation_model = "qwen3-0.6b"
piper_endpoint = "auto"
piper_voice = "auto"
segment_ms = 2000
silence_ms = 300
vad_threshold = 0.01
request_timeout_secs = 30

[local_runtime]
directory = ""
threads = 2
idle_unload_secs = 60

[microphone]
provider = "local"
source_language = "pt-BR"
target_language = "en-US"
prompt = "Preserve nomes próprios e termos técnicos."

[microphone.voice]
engine = "native"
voice_id = ""
style = ""
chunk_ms = 400
```

Preserve os dispositivos e outros campos existentes ao mesclar esse exemplo.
`piper_voice = "auto"` seleciona a voz do catálogo para o idioma de destino;
um `voice_id` explícito na rota tem prioridade. A cadeia não preserva
identidade vocal, não clona vozes e não diariza. O texto intermediário de
reconhecimento não alimenta o TXT: selecione um STT independente na página
**Transcrição**. Whisper STT e Whisper da tradução têm configuração própria.

Whisper Base Q5_1 é o padrão para novas configurações, com pesos de 59,7 MB.
Tiny Q5_1 usa 32,2 MB e Small Q5_1 usa 190,1 MB. As variantes originais continuam
selecionáveis e escolhas salvas são preservadas. O tradutor Qwen3 0.6B permanece
em Q8, com 639 MB; cada voz Piper medium ocupa cerca de 63–64 MB. Esses valores
são de download, não de RAM. Consulte os limites e a medição pontual no
[catálogo integrado](local-inference.md).

O reconhecimento é segmentado e a latência acumula reconhecimento, tradução
e síntese. Um modelo pequeno pode errar mais em frases ambíguas, idiomas pouco
representados ou contexto técnico. Filas são limitadas, e a máquina precisa
acompanhar o ritmo do áudio; não há promessa universal de tempo real em CPU.

### Armazenamento, vozes e servidores externos

O [guia de modelos locais](local-inference.md) descreve o catálogo, diretório
absoluto opcional, threads, primeira preparação e uso offline. A primeira
seleção precisa de internet para obter os pesos. Os executáveis dos motores
fazem parte do instalador; uma compilação de desenvolvimento precisa gerar o
pacote de runtimes antes de usar o modo integrado.

O padrão novo é de até dois threads para Whisper/Qwen, conforme CPUs disponíveis.
O limite configurável é 1–64; Piper conserva seu próprio controle interno.
`idle_unload_secs` aceita 1–3600 segundos, padrão 60: quando a última sessão
libera os motores, esse prazo permite reutilização antes de encerrar os
processos gerenciados e liberar a RAM. Os pesos continuam em cache no disco.
Salvar um provider numa função desligada prepara arquivos, mas não faz uma
sessão que só grava áudio carregar IA.

Os componentes podem usar **Servidor externo (avançado)** individualmente.
Informe o endpoint real de cada serviço; o Babel não descobre servidores por
portas padrão. O Whisper aceita multipart WAV em `/inference`. Para tradução,
`translation_api = "ollama"` usa a API de chat Ollama e `"openai"` usa chat
completions compatível. O Piper externo deve aceitar texto/voz e retornar WAV.
Endpoints remotos HTTPS recebem o áudio ou texto da etapa correspondente.
Servidores externos não são encerrados pela política de inatividade do Babel.

A API Ollama recebe mensagens de tradução, `stream=false`, `think=false`,
temperatura zero e limite de tokens. O modelo do servidor precisa aceitar esse
contrato; truncamento não é enviado à síntese. Consulte a
[API Ollama](https://docs.ollama.com/api/chat), o
[servidor Whisper](https://github.com/ggml-org/whisper.cpp/tree/master/examples/server)
e o [Piper HTTP](https://github.com/OHF-Voice/piper1-gpl/blob/main/docs/API_HTTP.md)
para manter servidores próprios.

Com uma voz TTS externa selecionada na rota, o tradutor local envia o texto
traduzido diretamente a esse sintetizador e não precisa produzir áudio com
Piper. Esse arranjo envia o texto ao provider de voz escolhido e exige sua chave.

### Desempenho e limites

`segment_ms` aceita 500–10000 ms; `silence_ms` aceita 100–2000 ms e precisa ser
menor que o segmento. O limiar RMS `vad_threshold` vai de 0.0001 a 0.5.
Aumentá-lo pode rejeitar ruído e também perder fala baixa. Há 100 ms anteriores
à detecção para reduzir o corte do começo das palavras. Os offsets referem-se
a segmentos, não a alinhamento de palavras.

Captura e inferência progridem em tarefas independentes, com no máximo dois
segmentos esperando inferência e dois áudios esperando reprodução por pipeline.
Duas rotas e uma transcrição independente aumentam carga de CPU e memória.
Se o serviço não acompanhar, a rota apresenta erro em vez de ampliar filas
indefinidamente. As chamadas têm timeout, teto de bytes e não seguem redirects.
Não há repetição automática de segmentos que possa duplicar fala.

O código Rust do Babel proíbe `unsafe` próprio. Os motores de inferência usam
bibliotecas nativas separadas, com suas próprias propriedades de segurança;
a integração não torna essas bibliotecas memory-safe. Licenças dos motores e
dos pesos são independentes e acompanham a distribuição/documentação do pacote.

### Validação disponível

```sh
cargo test --lib provider::openai
cargo test --lib provider::deepgram
cargo test --lib provider::local
```

Os testes usam um WebSocket e servidores HTTP locais reais como mocks: verificam
a confirmação de sessão antes do áudio, os dois protocolos OpenAI, PCM, texto,
alinhamento, multipart WAV, cadeia de tradução/síntese, limites, cancelamento e
saturação. Deepgram também tem mocks para autenticação, frames PCM, finais,
diarização, timestamps, keepalive, descarte da fila antiga e reconexão. Esses
testes não medem qualidade de modelos. Os testes não enviam voz para
nuvem e não usam chaves reais. Faça uma avaliação com áudio e idiomas de seu uso
após configurar credenciais ou concluir a preparação dos modelos locais.
