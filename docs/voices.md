# Biblioteca de vozes e síntese personalizada

O Babel oferece duas escolhas de saída. O caminho direto usa o áudio traduzido pelo provedor de fala. O caminho com voz personalizada recebe texto traduzido internamente e o sintetiza pela voz selecionada. Os arquivos de transcrição continuam contendo somente as falas originais; o texto traduzido usado pela síntese não deve ser gravado como transcrição original.

A síntese personalizada acrescenta uma etapa de IA, requisições e custo. O áudio sintetizado chega em streaming, mas precisa de algum texto antes de começar. Uma biblioteca de vozes não torna esse caminho equivalente à tradução direta contínua. É possível escolher uma voz diferente para cada direção; isso identifica a direção, não cada participante de uma chamada com áudio misturado.

## O que está implementado

| Recurso | Gemini | ElevenLabs |
|---|---|---|
| Listar biblioteca da conta | Vozes prontas e perfis personalizados | Vozes disponíveis para a chave |
| Criar voz por descrição | Perfil `prompted` persistente | Gera prévias e salva a primeira retornada |
| Clonar com referência enviada pelo usuário | Referência e gravação separada de consentimento | Instant Voice Cloning com referência |
| Síntese com voz selecionada | Gemini 3.8 Flash TTS ou Flash-Lite TTS | Modelo TTS configurado, por exemplo `eleven_flash_v2_5` |
| Prompt textual de estilo por síntese | Sim, via `speech_metadata.style` | Não neste adaptador; o campo precisa ficar vazio |
| Identificar várias pessoas no mesmo áudio | Não oferecido pela biblioteca | Não oferecido pela biblioteca |
| Cadastrar automaticamente a voz dos primeiros segundos | Não implementado | Não implementado |

O cliente usa o ID que a API realmente retorna. Não troca silenciosamente a voz escolhida por uma voz padrão. Uma resposta de cadastro não garante que o provedor já liberou a voz para síntese: a ElevenLabs pode retornar `verification_required`.

## Configurar credenciais

1. Crie uma chave no provedor e confirme acesso aos modelos e às operações de biblioteca.
2. Configure a chave no painel de credenciais do Babel ou no ambiente do processo que inicia o aplicativo.
3. Use nomes separados, por exemplo `GEMINI_API_KEY` e `ELEVENLABS_API_KEY`. Os perfis guardam o nome da credencial, não o segredo.
4. Escolha o provedor da biblioteca e carregue as vozes. Uma voz Gemini não pode ser usada como ID da ElevenLabs, ou vice-versa.
5. Aplique o perfil à rota desejada e reinicie a tradução para usar a nova configuração.

No Bash, é possível inserir uma chave sem registrá-la literalmente no histórico:

```bash
read -rsp 'Gemini API key: ' GEMINI_API_KEY
export GEMINI_API_KEY
```

O mesmo padrão funciona com `ELEVENLABS_API_KEY`. No Windows, configure a variável para o processo que abre o Babel ou use o painel. Chaves inseridas no painel ficam somente na memória da sessão e têm precedência sobre o ambiente. Reiniciar o aplicativo remove esse valor de memória. Os arquivos de configuração não persistem a chave.

As conexões cloud usam endpoints oficiais fixos e TLS. Redirecionamentos HTTP são recusados. Erros apresentados pelo Babel informam o código HTTP, sem reproduzir corpos de resposta ou cabeçalhos com segredos. A chave é marcada como sensível e a cópia obtida do resolvedor usa `Zeroizing`; isso não garante apagar todas as cópias internas das bibliotecas HTTP/TLS nem o ambiente do processo.

## Criar uma voz por descrição

Escolha um nome que facilite reconhecer o perfil e descreva os traços permanentes: timbre, faixa vocal, sotaque e estilo de locução. Um exemplo é “voz adulta, acolhedora, articulação clara e sotaque brasileiro neutro”. Use o idioma/código desejado, como `pt-BR`.

No Gemini, o cadastro usa `gemini-3.8-flash-tts`, `type=prompted` e armazenamento de perfil habilitado. O ID retornado pode ser reutilizado em sínteses. Um prompt de estilo por fala controla a interpretação sem redefinir o perfil. [Voice design](https://ai.google.dev/gemini-api/docs/voice-design).

Na ElevenLabs, a operação tem duas chamadas: gerar prévias e salvar um ID gerado. Esta versão do Babel salva a primeira prévia retornada, conforme indicado na tabela de recursos. A descrição mais o idioma deve ter 20–1000 caracteres. O áudio da prévia não é baixado: `stream_previews=true` pede somente os IDs, reduzindo a resposta. Não há uma etapa de audição/comparação de todas as prévias nesta interface. [Design](https://elevenlabs.io/docs/api-reference/text-to-voice/design), [salvar perfil](https://elevenlabs.io/docs/api-reference/text-to-voice/create).

Criar um perfil é uma ação externa explícita, com possível cobrança e consumo da cota de vozes. O Babel não repete automaticamente a criação após erros de rede, para evitar perfis duplicados. Se houver timeout depois do envio, recarregue a biblioteca antes de tentar novamente: a operação pode ter sido concluída remotamente.

## Clonar uma voz com referência

Os uploads aceitam **WAV RIFF, PCM16 sem compressão, mono, de 8 a 48 kHz**. Não basta renomear `.mp3` para `.wav`. O cliente verifica o cabeçalho, alinhamento, tamanho e duração. Use gravação limpa, sem músicas nem outras pessoas falando ao mesmo tempo.

Para Gemini, prepare:

- Referência de **10–30 segundos** da pessoa.
- Outro WAV contendo a frase de consentimento exigida pelo provedor, falada pela mesma pessoa. O Babel aceita 1–60 segundos para esse arquivo.
- A documentação recomenda 24 kHz e gravações em condições acústicas semelhantes. A API verifica o consentimento; validar o formato localmente não substitui essa verificação.

Frase oficial em português brasileiro:

> Eu sou o proprietário desta voz e autorizo o Google a usá-la para criar um modelo de voz sintética.

Os perfis Gemini persistentes têm limites e prazo de retenção definidos pela conta/provedor; consulte a documentação antes de depender de um ID permanentemente. [Requisitos de replicação](https://ai.google.dev/gemini-api/docs/voice-replication).

Para ElevenLabs, selecione a referência WAV. O adaptador aceita 1–300 segundos, sujeito ao limite do upload e às exigências do serviço. Ele usa Instant Voice Cloning; não implementa treinamento Professional Voice Cloning. O arquivo de consentimento separado é específico do fluxo Gemini e não é enviado na chamada ElevenLabs. Isso não elimina exigências de autorização, verificação ou acesso aplicadas à conta. [Instant Voice Cloning](https://elevenlabs.io/docs/api-reference/voices/ivc/create).

O formulário do painel aceita até **2 MiB por arquivo WAV**, tanto na referência quanto no consentimento. A API local limita a requisição JSON completa a **8 MiB**. Base64 aumenta o tamanho em aproximadamente um terço, e no Gemini os dois arquivos compartilham esse orçamento. O módulo também possui uma defesa interna de 16 MiB por arquivo decodificado; os limites menores do painel prevalecem. WAV mono de 24 kHz economiza espaço e atende à recomendação Gemini.

O Babel não captura amostras escondidas de participantes nem associa perfis clonados automaticamente a pessoas no áudio misturado. Os perfis são criados a partir dos arquivos explicitamente enviados e selecionados por rota.

## Seleção e reprodução

No modo de voz personalizada, informe provedor de síntese, modelo, credencial e ID de voz correspondentes. Modelos Gemini aceitos nesta integração:

```text
gemini-3.8-flash-tts
gemini-3.8-flash-lite-tts
```

Para ElevenLabs, um modelo de baixa latência como `eleven_flash_v2_5` pode ser configurado. A disponibilidade da voz/modelo depende da conta. O idioma `pt-BR` é convertido para o código `pt` usado pelo endpoint TTS; `eleven_multilingual_v2` detecta o idioma pelo texto, pois não aceita o parâmetro de idioma. O campo de estilo livre precisa ficar vazio para ElevenLabs. [Streaming TTS](https://elevenlabs.io/docs/api-reference/text-to-speech/stream).

O módulo emite PCM16 little-endian, mono, 24 kHz, em quadros de até 480 amostras, sem esperar baixar uma fala inteira. O motor controla o ritmo da reprodução pela quantidade real de amostras e conserva esse ritmo entre fragmentos de texto. O intervalo `voice.chunk_ms` limita a espera a partir do primeiro fragmento ainda não sintetizado, mesmo quando a fala continua. Frases são limitadas a 240 caracteres e a fila tem quatro trechos; se a síntese ficar atrasada além do orçamento configurado ou a fila encher, o fluxo termina com erro explícito. São verificados MIME, frequência quando informada, Base64 e continuidade dos pares de bytes. Áudio com cabeçalhos WAV/MP3/Ogg/FLAC não é tratado como PCM por engano. O Gemini precisa sinalizar conclusão do SSE; fechamento prematuro é erro. [Formato e streaming Gemini](https://ai.google.dev/gemini-api/docs/speech-generation#streaming-speech-generation).

## Endpoints usados

| Provedor | Operação | Endpoint |
|---|---|---|
| Gemini | Listar/criar perfis | `GET/POST https://generativelanguage.googleapis.com/v1beta/voices` |
| Gemini | Síntese em SSE | `POST https://generativelanguage.googleapis.com/v1beta/interactions` |
| ElevenLabs | Listar perfis | `GET https://api.elevenlabs.io/v2/voices` |
| ElevenLabs | Gerar design | `POST https://api.elevenlabs.io/v1/text-to-voice/design` |
| ElevenLabs | Salvar design | `POST https://api.elevenlabs.io/v1/text-to-voice` |
| ElevenLabs | Clonar referência | `POST https://api.elevenlabs.io/v1/voices/add` |
| ElevenLabs | Síntese PCM | `POST https://api.elevenlabs.io/v1/text-to-speech/{voice_id}/stream?output_format=pcm_24000` |

No código, as operações públicas são `voices::list`, `voices::design`, `voices::clone_voice` e `voices::synthesize`. O painel expõe biblioteca, design e clone em suas rotas autenticadas; use a interface para aproveitar os tokens e a proteção de origem do aplicativo.

Exemplo da estrutura de design enviada ao backend local, sem chave em texto claro:

```json
{
  "provider": "gemini",
  "api_key_env": "GEMINI_API_KEY",
  "name": "Português acolhedor",
  "description": "Voz adulta acolhedora, articulação clara e sotaque brasileiro neutro.",
  "language": "pt-BR"
}
```

Para clone, os campos são `provider`, `api_key_env`, `name`, `reference_base64` e `consent_base64` (obrigatório para Gemini). Envie somente o Base64 do WAV, sem prefixo `data:`. O retorno comum contém `id`, `name`, `provider` e `kind`.

## Custos, privacidade e limites operacionais

Biblioteca, cadastro e síntese usam a conta e cotas do provedor selecionado. Os valores de cobrança variam; o Babel não estima preços nem promete uma franquia gratuita. Se a tradução e a síntese usam fornecedores diferentes, o áudio original segue para o tradutor e o texto traduzido segue para o sintetizador. Referências vocais e consentimento vão para o serviço de cadastro quando o usuário cria um perfil. O perfil pode persistir na nuvem; o Babel não apaga perfis remotos ao remover uma seleção local.

O cliente de síntese pede `store=false` nas interações Gemini; o cadastro de perfil usa `store=true`. O significado de retenção, logs e políticas da conta continua sendo definido pelo provedor. Não se deve interpretar esses parâmetros como anonimização ou garantia geral de retenção zero.

Os limites locais protegem estabilidade: conexão HTTP de cinco segundos, requisição de até 60 segundos, resposta JSON de até 8 MiB, evento SSE de aproximadamente 1 MiB e áudio total de até 16 MiB por síntese. Uma fila de áudio bloqueada por dois segundos encerra a requisição. Bibliotecas são paginadas com limites de 100 páginas e 10.000 perfis; exceder o limite produz erro explícito, sem mostrar uma lista truncada como completa.

## Resolver problemas

| Sintoma | Ação |
|---|---|
| Credencial ausente | Configure a chave no painel ou no ambiente que inicia o Babel; confira o nome da variável do perfil. |
| HTTP 401/403 | Confira chave, permissões, modelo, região e acesso a operações de vozes. |
| HTTP 429 | Confira a cota/limite da conta; reduza sessões ou frequência de chamadas. |
| HTTP 400/422 | Confira modelo, ID, idioma, descrição e formato dos arquivos; a API pode exigir condições adicionais. |
| Perfil requer verificação | Complete o processo no provedor antes de selecionar a voz para síntese. |
| WAV inválido | Exporte áudio mono PCM16, sem compressão, com cabeçalho RIFF consistente. |
| Fila de síntese bloqueada | Reduza carga e duração dos fragmentos; confira se a saída física está consumindo o áudio. |
| Voz não aparece após timeout de cadastro | Recarregue a biblioteca; não repita imediatamente a criação. |
| Qualidade/voz varia entre participantes | A rota recebe áudio misturado; seleção de perfil não faz diarização. |

Os testes automatizados usam credenciais fictícias e servidores HTTP locais. Validam paginação, design em duas etapas, upload multipart, consentimento Gemini, SSE, PCM e sanitização de erros. Não comprovam acesso da sua conta nem a fidelidade da voz: esses pontos exigem uma chamada real explicitamente iniciada com sua chave.
