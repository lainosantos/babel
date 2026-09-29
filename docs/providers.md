# Provedores de fala

O `SpeechProvider` recebe PCM16 mono de 16 kHz em um canal limitado e devolve eventos de texto e, nos tradutores, PCM16 mono de 24 kHz. Cada direção que usa IA tem sua própria sessão. A passagem original e a gravação sem transcrição/tradução não abrem provedores. Rede, JSON, Base64 e IA ficam fora dos callbacks de áudio. Para adicionar um provedor, implemente o trait e registre sua factory.

## Perfis e escolha por direção

`providers.gemini`, `providers.openai`, `providers.elevenlabs` e `providers.local` guardam configurações independentes. `microphone.provider` e `speaker.provider` selecionam o tradutor de cada direção; `voice.engine` seleciona áudio nativo ou outro sintetizador. ElevenLabs participa como sintetizador, sem um endpoint speech-to-speech de tradução neste aplicativo. Chaves ficam na memória da sessão ou no ambiente; o TOML guarda somente o nome da credencial. Leia [configuração e operação](configuration.md), [OpenAI e pipeline local](other-providers.md) e [biblioteca de vozes](voices.md).

## Gemini

A integração usa o WebSocket oficial v1beta, TLS com validação de certificado e a chave indicada por `api_key_env`. A chave segue em `x-goog-api-key`, em vez da URL; o cabeçalho é marcado como sensível. O cliente não aceita endpoint configurável e nunca mostra corpos de erro, motivos de fechamento ou mensagens remotas em erros locais. A credencial é resolvida primeiro do armazenamento temporário do painel e depois do ambiente, usando o nome configurado em `providers.gemini.api_key_env`. A cópia resolvida usa `Zeroizing`; cópias internas da biblioteca HTTP/TLS e o ambiente do processo não têm garantia de apagamento.

Para tradução, há dois modos selecionados pelo modelo:

| Modelo | Comportamento | Configuração |
|---|---|---|
| `gemini-3.5-live-translate-preview` | Tradução contínua enquanto chega áudio; modelo padrão para o requisito de tempo real | Código BCP-47 de destino; idioma de origem automático. Voz, prompts, VAD e raciocínio não são enviados. `echoTargetLanguage=false`: silêncio quando a fala já está no idioma de destino. |
| `gemini-3.8-live` | Áudio em ambas as direções, com geração sujeita à detecção de atividade/turnos do modelo | Idiomas no prompt de interpretação, voz e duração de silêncio do VAD. `NO_INTERRUPTION` permite continuar capturando enquanto a tradução é reproduzida. |

O cliente não transforma `gemini-3.8-flash` em um modelo de fala nem promete tradução contínua no modo Live genérico. Disponibilidade e permissões dependem da conta Google. Modelos podem mudar; o nome permanece configurável. O prefixo `models/` é opcional. O modo específico de tradução só é ativado pelo identificador documentado, não por comparação parcial.

No modo contínuo, prompts não vazios causam erro de configuração. Não há envio de `clientContent`, texto ou marcadores de fim de turno. No modo genérico, o prompt orienta a interpretar perguntas/comandos capturados como conteúdo a traduzir. Isso é uma instrução ao modelo, sem garantia formal de resistência a comandos na fala.

A API recebe áudio binário PCM little-endian codificado em Base64, em `realtimeInput.audio`. A entrada começa somente depois de `setupComplete`. O motor usa quadros de 100 ms no modo de tradução contínua. A resposta pode trazer diversos fragmentos de áudio no mesmo evento; todos são processados. O resultado é reproduzido ao chegar, sem aguardar `turnComplete`. Esse evento serve somente para sinalizar o encerramento de uma geração aos consumidores.

As opções `input_transcription` e `output_transcription` da abstração são independentes e usam os campos de transcrição de `BidiGenerateContentSetup`. Quando a transcrição original está habilitada para uma direção, o aplicativo solicita a entrada: a fala original do microfone físico ou a fala original recebida pela saída virtual. Com voz TTS personalizada, solicita também a transcrição da tradução, mantida somente em memória para síntese. Apenas as falas originais são salvas. Fragmentos mantêm espaços e conteúdo exatamente como recebidos; o motor decide quando e onde gravá-los.

## Gemini: transcrição independente

Com a tradução da faixa desligada e sua transcrição ligada, o Babel usa somente
ASR. `providers.gemini.transcription_model` vazio seleciona
`gemini-3.5-transcribe-live`; também é possível informar esse identificador
explicitamente. A configuração solicita `TEXT` e modo `VERBATIM`, sem voz,
idioma de destino ou prompt de tradução. `source_language` aceita BCP-47 ou
`auto`. O modelo recebe PCM16 mono a 16 kHz e fornece texto original final.
Veja o [guia oficial de Live Transcribe](https://ai.google.dev/gemini-api/docs/live-api/live-transcribe).

O adaptador ignora hipóteses parciais para não duplicar conteúdo no TXT. A sessão
do modelo tem limite de dez minutos; `goAway` provoca reconexão controlada.
Não há diarização nem timestamps por palavra neste streaming. Encerrar a sessão
do Babel cancela reconhecimento pendente: espere uma pausa e o resultado final
quando precisar guardar a última frase. O writer esvazia apenas finais já
recebidos. A gravação WAV, quando habilitada, continua sendo um caminho separado.
Os [limites oficiais](https://ai.google.dev/gemini-api/docs/live-api/live-transcribe#limitations)
não equivalem a garantia de continuidade sem perda numa reconexão.

## Participantes, marcações de tempo e identidade vocal

Live Translate tenta reproduzir características vocais automaticamente. Essa capacidade pertence ao modelo; no caminho Live direto não há cadastro de voz, amostra inicial configurável ou `voice_id`. A Google documenta mudanças de voz após pausas e confusão durante trocas rápidas de locutor. Uma voz fixa, como `Kore` no modo Live genérico, também não clona a voz de entrada. [Limitações do Live Translate](https://ai.google.dev/gemini-api/docs/live-api/live-translate#limitations).

O áudio de uma chamada normalmente chega já misturado. Identificar a direção “microfone” ou “saída” não identifica cada pessoa desse sinal. Nomes atribuídos manualmente a um canal devem aparecer como rótulos do canal, nunca como identificação automática de locutor.

A existência de `diarization` e `wordTimestamp` no schema compartilhado de `AudioTranscriptionConfig` não confirma compatibilidade do modelo. A documentação de `gemini-3.5-transcribe-live` exclui diarização e timestamps por palavra no streaming; a API de transcrição de arquivos oferece esses recursos. Portanto, o provedor não envia essas opções nem inventa locutores ou alinhamentos temporais. [Guia Live Transcribe](https://ai.google.dev/gemini-api/docs/live-api/live-transcribe#limitations), [tabela de capacidades](https://ai.google.dev/gemini-api/docs/models/gemini-3.5-transcribe).

A abstração preserva metadados autênticos quando presentes: `speakerLabel` e os offsets da primeira/última palavra de `words` são expostos por `TranscriptMetadata`. IDs e listas têm limites; durações inválidas, negativas ou com overflow são rejeitadas. Campos ausentes continuam ausentes. Esses offsets são relativos ao áudio da sessão do provedor e podem reiniciar após reconexão. Essa compatibilidade com o schema do [SDK oficial](https://github.com/googleapis/python-genai/blob/main/google/genai/types.py#L2051) não ativa diarização nos modelos atuais.

`provider::capabilities(model)` declara apenas recursos confirmados para os modelos conhecidos. Gemini Live Translate informa tradução contínua e preservação vocal automática de melhor esforço; os modelos 3.8 Live informam voz fixa e prompts. OpenAI `gpt-realtime-translate` informa tradução contínua e `gpt-realtime-2.1` informa voz fixa e prompts, incluindo snapshots dessas famílias com sufixo de data válido. O catálogo não atribui preservação vocal automática ao OpenAI. Diarização, timestamps por palavra e cadastro de voz permanecem `false`: a biblioteca de vozes é um fluxo separado. Famílias desconhecidas e sufixos não reconhecidos não anunciam capacidades presumidas.

Marcações geradas pelo aplicativo a partir do relógio local indicam quando o texto foi recebido, incluindo o atraso da rede/IA. Não são o início de cada palavra no áudio. A documentação Live cita tempos de enunciados, mas a referência WebSocket pública não especifica esses offsets para Live Translate; o recebimento desses metadados não é garantido.

O Babel implementa uma biblioteca separada de design/clonagem e síntese com Gemini 3.8 Flash TTS ou ElevenLabs. No Gemini, o cadastro aceita 10–30 segundos de referência e uma gravação específica de consentimento da mesma pessoa, retornando um perfil reutilizável. Esse TTS recebe texto e não utiliza a Live API. O adaptador `revoice` mantém a tradução ativa, descarta seu áudio nativo e sintetiza o texto traduzido em blocos pela voz selecionada; portanto acrescenta requisições, latência e custo. Cada direção seleciona seu perfil, sem atribuir automaticamente clones a pessoas de uma chamada misturada. O caminho direto continua disponível para priorizar tradução contínua. Veja [biblioteca, requisitos e exemplos](voices.md), [Voice replication](https://ai.google.dev/gemini-api/docs/voice-replication) e [modelo TTS](https://ai.google.dev/gemini-api/docs/models/gemini-3.8-flash-tts).

## Limites e recuperação

- Mensagens WebSocket: até 512 KiB; fragmento PCM de saída: até 48.000 bytes (um segundo); entrada: até 16.000 amostras por envio. MIME, frequência, canais, Base64 e comprimento par de PCM16 são validados.
- Envio de áudio: prazo de 500 ms. Eventos de reprodução: prazo de dois segundos. Filas e buffers de rede são limitados. Congestionamento prolongado encerra a sessão, em vez de consumir memória sem limite.
- Ping a cada 15 segundos; conexão sem nenhuma resposta por 45 segundos é reaberta. Um segundo sem novos quadros envia `audioStreamEnd`, sem encerrar a conexão.
- O número configurado de tentativas limita falhas consecutivas; uma sessão saudável de pelo menos um minuto restaura o orçamento. A espera aumenta de 250 ms até cinco segundos. Erros de configuração, autenticação e protocolo não geram retries infinitos.
- Ao reconectar, a reprodução pendente é interrompida e o áudio capturado durante a indisponibilidade é descartado. Não há promessa de continuidade sem perda durante falhas da rede ou rotação de sessão.
- No Live genérico, `sessionResumptionUpdate` e `goAway` permitem retomar de um ponto explicitamente resumível. O cliente habilita compressão de contexto. No modelo de tradução, a sessão é recriada sem essas opções, cuja compatibilidade específica não está documentada.
- Cancelamento interrompe conexão, espera de setup, rede e filas. O fechamento inesperado da captura é erro, não conclusão bem-sucedida.

## Diagnóstico local

O provedor `loopback` testa o caminho de áudio sem rede, chave ou IA. Ele repassa a fala com interpolação linear de 16 kHz para 24 kHz, preservando fase entre quadros. Não traduz e não representa a qualidade sonora/latência do Gemini.

Os testes usam um servidor WebSocket local com credencial fictícia: barreira de setup, PCM little-endian, múltiplos fragmentos, transcrições, cancelamento, retomada de sessão, orçamento de retries, erros sanitizados e EOF de captura. Eles não comprovam autorização da conta nem a qualidade real da tradução; isso exige uma chave válida e áudio real.

## Referências oficiais

Documentação consultada em 29/09/2026:

- [Live Translation](https://ai.google.dev/gemini-api/docs/live-api/live-translate): modo contínuo, modelo especializado e configuração.
- [Referência WebSockets](https://ai.google.dev/api/live): mensagens, transcrição, setup e retomada.
- [Capacidades Live](https://ai.google.dev/gemini-api/docs/live-api/capabilities): formatos PCM e capacidades do modelo 3.8 Live.
- [SDK oficial: cabeçalhos](https://github.com/googleapis/python-genai/blob/main/google/genai/_api_client.py) e [conexão Live](https://github.com/googleapis/python-genai/blob/main/google/genai/live.py): autenticação por cabeçalho.
