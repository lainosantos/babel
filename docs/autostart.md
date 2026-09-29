# Abrir o Babel ao entrar no sistema

A opção **Iniciar com o sistema** abre a bandeja e o painel do Babel no login do
usuário. Ela vem **desativada**. Ao abrir, o Babel encaminha o áudio
original entre os dispositivos configurados quando um aplicativo usa o dispositivo
virtual correspondente. Tradução, transcrição e gravação
aguardam **Iniciar sessão**; o encaminhamento original não envia áudio aos
provedores nem cria arquivos. O início automático também não instala dispositivos
virtuais.

No painel, marque a opção e clique em **Aplicar**. Para desativar, desmarque e
aplique novamente. Consultar a opção ou salvar outras configurações não altera
a inicialização do sistema. A mudança vale para o próximo login e não encerra
nem inicia outra instância imediatamente.

O painel identifica o sistema operacional do processo Babel e mostra a
integração de login correspondente: serviço systemd de usuário ou XDG Autostart
no Linux, LaunchAgent no macOS
ou a entrada Run do usuário no Windows. O navegador e o idioma escolhido não
alteram essa identificação. A tabela abaixo documenta os três sistemas para
consulta; a configuração exibida pelo aplicativo se refere ao sistema atual.

## Arquivos e localização do programa

Compile os dois executáveis:

```sh
cargo build --release --bins
```

No Windows, distribua `babel.exe` e `babel-tray.exe` no mesmo diretório.
`babel-tray.exe` usa o subsistema gráfico do Windows: a inicialização não abre uma
janela de console. `babel` continua disponível para os comandos de terminal.
O fallback XDG no Linux e o macOS registram o executável que está sendo usado;
ambos também podem executar `babel-tray` diretamente. O serviço Linux usa o
launcher instalado pelo pacote em `/usr/bin/babel-launch`.

Ative a opção somente depois de colocar o programa em seu diretório definitivo.
A entrada guarda caminhos absolutos do executável e da configuração, `--port 0`
e o diretório de trabalho atual. O sistema escolhe uma porta livre a cada login;
a entrada nunca guarda a porta temporária da execução atual. O diretório mantém os caminhos relativos da
configuração consistentes após o login. Se mover o programa, a configuração ou
o diretório de trabalho, abra o Babel no local definitivo e aplique a opção
novamente. Uma cópia em `target/debug` depende desse diretório continuar existindo.

A entrada de login não contém chaves de API, o token do painel, transcrições ou
áudio. O token do painel é gerado novamente a cada execução. Credenciais
informadas apenas na memória do processo precisarão ser informadas novamente;
variáveis de ambiente precisam estar disponíveis na sessão gráfica, que pode
não carregar o perfil do seu terminal.

## Integração por sistema

| Sistema | Registro por usuário | Comportamento |
| --- | --- | --- |
| Linux, pacote DEB/RPM e sessão compatível | Unidade `/usr/lib/systemd/user/org.babel.audio.service`; configuração pessoal em `$XDG_CONFIG_HOME/systemd/user/org.babel.audio.service.d/50-babel.conf` | Serviço systemd do usuário, vinculado a `graphical-session.target`. |
| Linux, fallback | `$XDG_CONFIG_HOME/autostart/org.babel.audio.desktop`, ou `~/.config/autostart/org.babel.audio.desktop` | XDG Autostart da sessão gráfica; `Terminal=false`. |
| macOS | `~/Library/LaunchAgents/org.babel.audio.plist` | LaunchAgent da sessão Aqua com `RunAtLoad`; sem `KeepAlive`, sem reinício contínuo. |
| Windows | Valor `BabelAudio` em `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` | Abre `babel-tray.exe`, com diretório de trabalho explicitamente passado ao launcher. |

Ativar ou desativar o login no aplicativo não precisa de administrador ou rede.
Instalar o pacote DEB/RPM é uma operação separada, que requer as permissões do
gerenciador de pacotes.
Desativar remove somente a entrada marcada como pertencente ao Babel; entradas
estranhas com o mesmo nome e links simbólicos são recusados em vez de
sobrescritos. O aplicativo verifica novamente a propriedade da entrada antes
de uma alteração e salva os arquivos por substituição atômica.

### Serviço de usuário no Linux

Os pacotes DEB e RPM incluem a unidade systemd, mas **não a habilitam nem a
iniciam durante a instalação**. Não há serviço systemd de sistema, execução como
root ou mudança de configuração de áudio pelo instalador.

Ao aplicar a opção, o Babel prefere o serviço quando a unidade do pacote está
presente, íntegra e carregada pelo gerenciador de usuário, o alvo
`graphical-session.target` está ativo e o ambiente desse gerenciador contém
`DISPLAY` ou `WAYLAND_DISPLAY`. A disponibilidade é verificada no processo
Linux; ter essas variáveis somente no terminal não basta. Desktops sem essa
integração, instalações portáteis sem a unidade e sessões sem systemd usam XDG.
Um serviço já habilitado permanece visível e pode ser desabilitado mesmo se o
alvo gráfico não estiver ativo no momento da consulta.

O campo **Método de inicialização** indica o método efetivamente registrado, e
o caminho mostra a entrada pessoal administrada pelo Babel. Uma entrada XDG
existente permanece indicada como habilitada até você aplicar a opção: quando
o serviço estiver disponível, o Babel cria o drop-in pessoal, habilita somente
essa unidade e então remove sua própria entrada XDG para evitar duplicidade.

O drop-in passa ao launcher o caminho absoluto da configuração atualmente
aberta, `--port 0` e o diretório de trabalho. Argumentos são escapados conforme
systemd; expansão de variáveis é desativada e percentuais são literais. Não há
shell intermediário interpretando esses argumentos. Sem `XDG_CONFIG_HOME`
absoluto, a configuração pessoal fica em `~/.config/systemd/user/`.

Habilitar cria o vínculo pessoal em
`graphical-session.target.wants/org.babel.audio.service`; desabilitar remove a
habilitação e preserva o drop-in para diagnóstico e configuração. O Babel usa
`enable`/`disable` sem `--now`: **não inicia, para nem reinicia o processo atual**.
`daemon-reload` apenas atualiza a configuração do gerenciador. Depois de um
login que inicie o serviço, uma falha do processo permite reinício limitado por
`Restart=on-failure`; sair normalmente não causa reinício. Ao encerrar a sessão
gráfica, a unidade usa `SIGINT` para permitir o fechamento dos arquivos da sessão.

Unidades alteradas, habilitação global administrada fora do Babel e overrides
não administrados pelo aplicativo geram diagnóstico, em vez de serem
sobrescritos. Se uma habilitação systemd existente não puder ser consultada,
não é criada uma entrada XDG adicional. Consulte a configuração a partir de
uma sessão gráfica funcional e aplique novamente. O Babel verifica identidade
e conteúdo do pacote, mas não administra outros serviços nem importa o ambiente
do terminal para systemd.

Comandos de diagnóstico, sem mudar a inicialização:

```sh
systemctl --user status org.babel.audio.service
systemctl --user is-enabled org.babel.audio.service
systemctl --user show graphical-session.target --property=ActiveState
```

### Outras integrações e fallback XDG

No Linux, um `XDG_CONFIG_HOME` relativo é ignorado, conforme a convenção XDG.
Caminhos de argumentos são escapados sem executar shell. O padrão `.desktop`
não aceita `=` no caminho do executável; nesse caso mova o programa. Para um
executável cujo nome contém `%`, o launcher usa `/usr/bin/env` para contornar a
resolução antecipada de nomes no GLib, sem interpretar os argumentos como código.

No Windows, a chave Run tem limite documentado de 260 caracteres por comando.
Se a combinação de caminhos exceder o limite, o Babel recusa a alteração e
orienta a usar caminhos menores, sem truncar o comando. O Windows pode adiar ou
bloquear aplicativos de inicialização por preferências do usuário/organização;
confira **Configurações → Aplicativos → Inicialização**. O estado exibido no Babel
confirma o registro que ele administra, não uma autorização externa do sistema.

No macOS, o aplicativo grava o LaunchAgent para o próximo login; não executa
`launchctl bootstrap` no meio da sessão. Desativar não mata o processo atual.
Permissões e políticas de execução do macOS continuam aplicáveis ao binário.

A bandeja depende do ambiente desktop. Se estiver indisponível no Linux, o
painel local continua disponível pelo endereço impresso no terminal. **Configurações**
na bandeja sempre abre o endereço e token da execução atual. Favoritos com a porta
de uma execução anterior deixam de ser válidos.

O Babel mantém o socket aberto desde a escolha da porta; não consulta uma porta
livre para reservá-la depois. `babel serve --port NUMERO` permite solicitar uma
porta específica: se ela estiver ocupada, o Babel avisa no log e abre seu painel
em uma porta livre escolhida pelo sistema, sem encaminhar o usuário ao outro
programa. Novas entradas de login usam sempre `--port 0`; reaplique a opção para
atualizar uma entrada criada por uma versão anterior.

Uma trava do sistema impede duas instâncias com a mesma configuração, incluindo
`babel run`. Ela fica no arquivo auxiliar `<configuração>.babel-instance.lock`,
ao lado do TOML, e permanece adquirida até encerrar o controlador e os fluxos.
Uma segunda inicialização mostra a orientação para usar a instância existente;
não inicia outro roteamento. O arquivo não contém chaves nem token e não deve ser
apagado enquanto o Babel está aberto. Ao sair, o sistema libera a trava; o arquivo
vazio permanece para que a próxima execução possa reutilizá-lo. A pasta da
configuração precisa permitir a criação desse arquivo.

## Verificação

Os testes de criação, atualização, remoção, propriedade e links simbólicos usam
somente diretórios temporários. Há testes de escape XML/plist, argumentos
Windows e limite de comprimento. Nenhum teste ativa a inicialização no login
real do desenvolvedor.

Os testes systemd usam um gerenciador simulado e diretórios temporários para
validar identidade da unidade, fallback sem sessão gráfica/ambiente de display,
migração XDG, configuração personalizada, rollback e recusa de overrides. Eles
também verificam que nunca são enviados `start`, `stop`, `restart` ou `--now`.
O coletor de saída é testado com processos temporários sem chamar systemd;
há limites de memória e tempo. O ambiente consultado pelo aplicativo não é
registrado em logs, persistido ou devolvido ao painel.

Os testes do painel abrem sockets apenas em loopback para conferir portas
dinâmicas, colisões, token, Host e Origin; não iniciam captura nem roteamento.
Os testes da trava usam configurações temporárias e verificam que salvar o TOML
por substituição atômica não permite uma segunda instância.

```sh
cargo test --lib autostart::
cargo test --lib dashboard::
```

No Linux com `gio` e `desktop-file-validate`, este teste adicional valida um
`.desktop` temporário e executa somente um gravador temporário de argumentos:

```sh
cargo test --lib autostart::tests::desktop_launcher_preserves_actual_arguments_through_gio -- --ignored --nocapture
```

Ele verifica o trajeto real de argumentos com espaços, Unicode, percentuais,
aspas, barras, cifrões e crases. Testar o login efetivo exige uma sessão real do
desktop Linux, macOS ou Windows correspondente; testes simulados e cross-check
de compilação não substituem essa execução.

Referências primárias: [XDG Autostart](https://specifications.freedesktop.org/autostart/latest/),
[escape de Exec](https://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html),
[integração de desktops com systemd](https://systemd.io/DESKTOP_ENVIRONMENTS/),
[systemctl enable/disable](https://www.freedesktop.org/software/systemd/man/latest/systemctl.html),
[sintaxe ExecStart de systemd](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html),
[LaunchAgents da Apple](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html),
[Run/RunOnce do Windows](https://learn.microsoft.com/en-us/windows/win32/setupapi/run-and-runonce-registry-keys)
e [argumentos Windows](https://learn.microsoft.com/en-us/cpp/c-language/parsing-c-command-line-arguments).
