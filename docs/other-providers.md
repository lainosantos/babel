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

## Provider local: whisper.cpp → Ollama → Piper

O provider `local` é uma implementação funcional de três chamadas HTTP locais:

1. A captura contínua detecta atividade por energia e produz segmentos curtos.
2. whisper.cpp reconhece o áudio original a partir de WAV mono PCM16/16 kHz.
3. Ollama traduz o texto por mensagens com instruções de tradução.
4. Piper sintetiza a tradução em uma voz instalada; Babel converte seu WAV
   para 24 kHz e o entrega em frames de 20 ms.

Essa cadeia tem latência de segmentação, reconhecimento, tradução e síntese.
Ela não preserva automaticamente a voz original, não clona vozes e não faz
diarização. Os segmentos podem cortar uma frase longa no limite configurado.
Os offsets do reconhecimento interno dessa cadeia correspondem aos segmentos
capturados, não a palavras. Esse texto intermediário não alimenta o TXT; para
gravar uma transcrição, configure o reconhecedor STT independente. A tradução tem um ponto de
alinhamento ao segmento de origem, sem duração inventada de fala sintetizada.

Os serviços e pesos são instalados separadamente. O Babel não baixa modelos
silenciosamente. Após baixar os arquivos necessários, os endpoints de loopback
não precisam de chave de nuvem. Se você mudar um endpoint para um servidor
HTTPS remoto, áudio/texto passam a ser enviados a esse servidor.

### Whisper: transcrição independente

Escolha `whisper` na página **Transcrição** e configure
`transcription.providers.whisper`, incluindo seu endpoint HTTP explícito.
O adaptador envia `translate=false` e reconhece os originais sem Ollama, Piper,
modelo de tradução ou perfil `providers.local`. O idioma de cada origem pertence
a `transcription.microphone_recognition.language` ou
`transcription.speaker_recognition.language`.

O STT funciona com qualquer tradutor ou com tradução desligada. Mesmo quando
Whisper participa da cadeia local de tradução, o STT é uma tarefa independente;
apontar ambos para o mesmo servidor aumenta a carga de inferência. A segmentação
do STT tem seus próprios limites, e seus tempos representam segmentos capturados,
não palavras ou falantes. Encerrar a sessão cancela segmentos ainda em
reconhecimento; resultados finais já entregues são gravados. Veja os campos,
a autenticação opcional e os exemplos no [guia de transcrição](transcription.md).

### Pré-requisitos por sistema

| Sistema | Compilação whisper.cpp | Python/Piper | Ollama |
|---|---|---|---|
| Linux | Git, CMake e compilador C/C++ | Python 3 com `venv` e pip | instalador oficial Linux |
| macOS | Xcode Command Line Tools, Git e CMake | Python 3 com `venv` e pip | aplicativo oficial macOS |
| Windows | Git, CMake e Visual Studio Build Tools com C++ | Python 3 e launcher `py` | instalador oficial Windows |

Exemplos de dependências de desenvolvimento:

```sh
# Debian/Ubuntu
sudo apt install build-essential cmake git python3-venv python3-pip

# macOS, após instalar Homebrew
xcode-select --install
brew install cmake git python
```

No Windows, instale CMake/Git/Python e a carga de trabalho C++ do Visual Studio
Build Tools. Use o Developer PowerShell para os comandos CMake abaixo. Esses
comandos são instruções de instalação; não foram executados automaticamente
pelo Babel.

### 1. whisper.cpp

Linux/macOS, em uma pasta de ferramentas fora do projeto:

```sh
git clone https://github.com/ggml-org/whisper.cpp.git
cd whisper.cpp
sh models/download-ggml-model.sh base
cmake -B build -DWHISPER_BUILD_SERVER=ON -DCMAKE_BUILD_TYPE=Release
cmake --build build --config Release --parallel
./build/bin/whisper-server -m models/ggml-base.bin --host 127.0.0.1 --port 8080 -l auto
```

Windows, Developer PowerShell:

```powershell
git clone https://github.com/ggml-org/whisper.cpp.git
Set-Location whisper.cpp
.\models\download-ggml-model.cmd base
cmake -B build -DWHISPER_BUILD_SERVER=ON
cmake --build build --config Release --parallel
.\build\bin\Release\whisper-server.exe -m models\ggml-base.bin --host 127.0.0.1 --port 8080 -l auto
```

Use um modelo multilíngue como `base` ou `small`, sem o sufixo `.en`, para
reconhecer português e outros idiomas. O servidor precisa permanecer executando;
o endpoint padrão é `http://127.0.0.1:8080/inference`. O Babel já envia WAV
compatível e não precisa ativar conversão via ffmpeg. Build e opções de aceleração
estão no [projeto whisper.cpp](https://github.com/ggml-org/whisper.cpp);
o contrato multipart é descrito no [servidor HTTP](https://github.com/ggml-org/whisper.cpp/tree/master/examples/server).

### 2. Ollama

Instale o [Ollama para seu sistema](https://ollama.com/download). Em qualquer um
dos três sistemas, baixe o modelo configurado e deixe o serviço ativo:

```sh
ollama pull qwen3:4b
# Só execute serve se o aplicativo/serviço ainda não estiver escutando na porta.
ollama serve
```

O modelo padrão é [qwen3:4b](https://ollama.com/library/qwen3:4b), escolhido aqui
como uma opção local configurável. Isso não representa benchmark de qualidade
ou garantia de desempenho no seu hardware. O adaptador usa `/api/chat`,
`stream=false`, `think=false`, temperatura zero e limite de tokens para segmentos
curtos. Um modelo alternativo deve aceitar esses parâmetros. A resposta deve
terminar normalmente; truncamento não é enviado à síntese. Consulte a
[API de chat](https://docs.ollama.com/api/chat).

Ao selecionar uma voz externa (por exemplo ElevenLabs ou Gemini TTS), a cascata
local envia diretamente o texto traduzido a esse sintetizador: não chama Piper
nem produz um WAV intermediário. Nesse modo, Piper não precisa estar em execução.

### 3. Piper

Crie um ambiente Python. Linux/macOS:

```sh
python3 -m venv .venv-piper
. .venv-piper/bin/activate
python -m pip install 'piper-tts[http]'
python -m piper.download_voices en_US-lessac-medium pt_BR-faber-medium
python -m piper.http_server -m en_US-lessac-medium --host 127.0.0.1 --port 5000
```

Windows, PowerShell, sem depender da ativação do ambiente:

```powershell
py -3 -m venv .venv-piper
.\.venv-piper\Scripts\python.exe -m pip install "piper-tts[http]"
.\.venv-piper\Scripts\python.exe -m piper.download_voices en_US-lessac-medium pt_BR-faber-medium
.\.venv-piper\Scripts\python.exe -m piper.http_server -m en_US-lessac-medium --host 127.0.0.1 --port 5000
```

Baixe as vozes e execute o servidor no mesmo diretório, ou configure `--data-dir`.
Confira os nomes disponíveis em `http://127.0.0.1:5000/voices`. Babel envia
`{"text":"...","voice":"..."}` a `/synthesize`; uma voz vazia usa o padrão
do servidor. Documentação do [Piper HTTP](https://github.com/OHF-Voice/piper1-gpl/blob/main/docs/API_HTTP.md).
A voz brasileira de exemplo está no [catálogo Faber](https://huggingface.co/rhasspy/piper-voices/tree/main/pt/pt_BR/faber/medium).

### 4. Configuração no Babel

Mescle estes campos na configuração existente, preservando os dispositivos:

```toml
[providers.local]
whisper_endpoint = "http://127.0.0.1:8080/inference"
ollama_endpoint = "http://127.0.0.1:11434/api/chat"
translation_model = "qwen3:4b"
piper_endpoint = "http://127.0.0.1:5000/synthesize"
piper_voice = ""
segment_ms = 2000
silence_ms = 300
vad_threshold = 0.01
request_timeout_secs = 30

[microphone]
provider = "local"
source_language = "pt-BR"
target_language = "en-US"
prompt = "Preserve nomes próprios e termos técnicos."

[microphone.voice]
engine = "native"
voice_id = "en_US-lessac-medium"
style = ""
chunk_ms = 400

[speaker]
provider = "local"
source_language = "en-US"
target_language = "pt-BR"
prompt = ""

[speaker.voice]
engine = "native"
voice_id = "pt_BR-faber-medium"
style = ""
chunk_ms = 400
```

A voz escolhida deve falar o idioma de destino. `piper_voice` é o fallback para
rotas sem `voice_id`; ambos vazios usam o padrão do servidor. `segment_ms` aceita
500–10000 ms, `silence_ms` aceita 100–2000 ms e precisa ser menor que o segmento.
`vad_threshold` é RMS normalizado entre 0.0001 e 0.5: aumentá-lo rejeita mais
ruído e também pode perder fala baixa. Há 100 ms de áudio anterior à detecção
para reduzir o corte do começo das palavras. Esses limites descrevem o código
do Babel, não garantias de reconhecimento do Whisper.

### Desempenho, erros e controle

Capture e inferência progridem em tarefas independentes. Há no máximo dois
segmentos esperando inferência e dois áudios esperando reprodução. A reprodução
é cadenciada; não despeja um WAV inteiro na fila do dispositivo. Se a máquina
não acompanhar a entrada, a rota encerra com erro explícito em vez de aumentar
a fila indefinidamente. Duas rotas simultâneas dividem CPU/GPU e serviços; teste
primeiro uma rota.

CPU funciona para os componentes que suportarem seu sistema, mas uma GPU pode
ser necessária para acompanhar fala contínua. O tempo de carregar os modelos
também afeta o primeiro segmento. Modelos menores e segmentos maiores podem
reduzir pressão de processamento com trocas de qualidade/latência. Verifique
as opções oficiais do [whisper.cpp](https://github.com/ggml-org/whisper.cpp) e o
[suporte de hardware Ollama](https://docs.ollama.com/gpu); o projeto não afirma
números de latência, uso de RAM ou requisitos mínimos universais.

Cada chamada HTTP tem timeout, não segue redirecionamentos e tem teto de bytes.
Falhas de serviço, resposta malformada, WAV excessivo e filas cheias aparecem
como erro da rota. Não há repetição automática de segmentos, que poderia
reproduzir fala duplicada. Endpoints HTTP são aceitos apenas no loopback; outros
hosts precisam de HTTPS. Credenciais, query e fragmentos em URLs são rejeitados.
O cliente se conecta diretamente, sem usar proxies HTTP do ambiente.

O núcleo Babel proíbe `unsafe` no próprio código. whisper.cpp, runtimes de
modelos, servidores Python, bibliotecas nativas e drivers são componentes
separados com suas próprias propriedades de segurança e memória. O motor atual
[Piper](https://github.com/OHF-Voice/piper1-gpl) é GPLv3; pesos de voz/modelos
podem ter licenças diferentes. A instalação separada aqui não redistribui esses
componentes dentro do executável Babel. Consulte as licenças dos arquivos que
você efetivamente selecionar para distribuir um produto.

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
após configurar credenciais ou instalar e iniciar os serviços locais.
