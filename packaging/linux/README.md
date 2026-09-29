# Pacotes Linux

Este diretório gera os pacotes **amd64/x86-64 com glibc** do aplicativo Babel.
Não inclui driver de kernel: o backend Linux usa o servidor PulseAudio ou
PipeWire com `pipewire-pulse` da sessão do usuário.

## Compilar e gerar

Requisitos de empacotamento: Python **3.11+**, `dpkg-deb` do pacote `dpkg`,
`readelf` de `binutils`, `rpmbuild`/`rpm` de `rpm` e `rpm2cpio` (RPM **4.17+**).
`cpio` é útil para inspeção manual; a validação do builder lê o payload CPIO
diretamente, sem extrair caminhos não validados. O builder não compila Rust,
instala dependências nem executa os binários recebidos. No runner Ubuntu 22.04:

```sh
sudo apt install dpkg binutils rpm rpm2cpio cpio
cargo build --release --locked --bins
python3 packaging/linux/build.py --bin-dir target/release --output artifacts
python3 -m unittest discover -s packaging/linux -p 'test_*.py' -v
```

O script lê a versão de `Cargo.toml`. `--version 1.2.3` substitui essa versão;
pré-lançamentos como `1.2.3-rc.1` usam `1.2.3~rc.1` no `.deb` e no `.rpm`,
respeitando a ordenação antes da versão final. `SOURCE_DATE_EPOCH` controla os timestamps dos arquivos;
sem a variável, o timestamp é zero. Para uma versão de exemplo `0.1.0`, gera:

- `babel-audio_0.1.0_amd64.deb`;
- `babel-audio-0.1.0-1.x86_64.rpm`;
- `babel-audio-0.1.0-linux-amd64.tar.gz`;
- `babel-audio-0.1.0-linux-amd64.manifest.json`, com SHA-256 dos artefatos.

A geração inspeciona os dois executáveis como ELF amd64 e extrai seus requisitos
de glibc. Uma biblioteca dinâmica desconhecida interrompe o build até receber
um mapeamento de dependência explícito. Gerar em Ubuntu 22.04 amplia a compatibilidade;
compilar em uma distribuição mais nova pode exigir glibc mais nova também.
O mínimo efetivamente medido aparece no manifesto, no `Depends` do `.deb` e no
`Requires` do `.rpm`.
O tarball tem o mesmo requisito, **não é um binário estático para musl/Alpine**.

O builder valida `dpkg-deb --info`, `--contents`, os metadados e dependências do
RPM, todos os hashes/permissões dos três formatos e a ausência de hooks,
scriptlets e triggers de instalação/remoção. O RPM usa formato 4 com payload gzip;
o build preserva os executáveis recebidos, sem stripping ou pós-processamento.
As duas gerações independentes do teste devem produzir artefatos idênticos com
os mesmos arquivos, versões das ferramentas e `SOURCE_DATE_EPOCH`.
Os testes usam executáveis de sistema como fixtures, sem iniciá-los. O teste do
launcher usa um stub que só registra argumentos, sem iniciar Babel ou áudio.

## Instalar o .deb em Debian/Ubuntu

```sh
sudo apt install ./babel-audio_0.1.0_amd64.deb
```

O pacote instala `babel`, `babel-tray` e `babel-launch` em `/usr/bin`, uma entrada
**Babel** no menu de aplicativos e o ícone. Documentos e o manifesto do conteúdo
ficam em `/usr/share/doc/babel-audio`; o helper opcional Needle fica em
`/usr/share/babel/scripts/needle_bridge.py`.

Dependências declaradas: glibc compatível com o build, `libgcc-s1` quando usado
pelos binários, `pulseaudio-utils`, `xdg-utils` e um provedor de D-Bus de sessão.
O servidor `pipewire-pulse` ou `pulseaudio` aparece somente como **Suggests**:
o pacote Babel não deve escolher, substituir ou iniciar seu servidor de áudio.
Use o servidor já configurado no desktop. A bandeja precisa de suporte
StatusNotifier/AppIndicator; no GNOME esse suporte pode depender de uma extensão.

A instalação não executa Babel, cria dispositivos, ativa tradução ou habilita
início automático. Não há `postinst`, `prerm` nem entrada em `/etc/xdg/autostart`.
A unidade systemd **de usuário** é instalada desabilitada em
`/usr/lib/systemd/user/org.babel.audio.service`. Criar os dispositivos e habilitar
início automático são ações separadas do usuário no aplicativo.

## Instalar o .rpm em Fedora e outras distribuições RPM

```sh
sudo dnf install ./babel-audio-0.1.0-1.x86_64.rpm
```

Em openSUSE, use `sudo zypper install ./babel-audio-0.1.0-1.x86_64.rpm`.
O RPM instala os mesmos arquivos, launcher e unidade de usuário do DEB.
Declara glibc compatível, a biblioteca `libgcc_s.so.1` quando usada, `/bin/sh`,
D-Bus e as ferramentas pelos caminhos `/usr/bin/pactl`, `/usr/bin/parec`,
`/usr/bin/pacat` e `/usr/bin/xdg-open`. O gerenciador resolve os pacotes que
fornecem esses caminhos. Um desktop com servidor PulseAudio/PipeWire-pulse e
suporte à bandeja deve já estar configurado. O RPM não escolhe nem inicia o
servidor de áudio, e não possui scriptlets ou triggers de instalação/remoção.

## Serviço de usuário e início automático

Somente a instalação DEB/RPM coloca a unidade no diretório de unidades do
sistema; ela continua sendo um serviço do **usuário**, nunca um daemon root.
Não use `sudo systemctl` nem habilite linger para este aplicativo gráfico.
No painel do Babel, o ajuste de início automático pode habilitar/desabilitar
a unidade empacotada; ele não inicia ou para a instância atual. O drop-in do
aplicativo preserva o caminho absoluto da configuração escolhida.

Para controlar manualmente, na sessão gráfica do próprio usuário:

```sh
systemctl --user daemon-reload
systemctl --user enable org.babel.audio.service   # somente próximos logins
systemctl --user status org.babel.audio.service
systemctl --user disable org.babel.audio.service # não encerra a instância atual
```

Para iniciar agora, primeiro encerre o Babel que já estiver aberto e use
`systemctl --user start org.babel.audio.service`. Para encerrá-lo, use
`systemctl --user stop org.babel.audio.service` ou **Sair** na bandeja.
Diagnósticos: `journalctl --user -u org.babel.audio.service`.

A unidade é vinculada a `graphical-session.target` e exige `DISPLAY` ou
`WAYLAND_DISPLAY` no ambiente do gerenciador de usuário; desktops sem essa
integração devem usar a opção de autostart XDG do aplicativo. Ela não inicia
em sessão root nem no boot sem login gráfico. Falhas reiniciam o processo
com um limite de tentativas; sair normalmente não reinicia. Ao parar, recebe
SIGINT para finalizar a sessão e os arquivos, com limite de 15 segundos.

## Usar o tarball em outras distribuições

```sh
tar -xzf babel-audio-0.1.0-linux-amd64.tar.gz
./babel-audio-0.1.0-linux-amd64/bin/babel-launch
```

Instale as ferramentas PulseAudio da sua distribuição (`pactl`, `parec`, `pacat`),
`xdg-open` e D-Bus de sessão. Em Debian/Ubuntu, se preferir o tarball:

```sh
sudo apt install pulseaudio-utils xdg-utils dbus-user-session
```

Verifique no manifesto a glibc mínima e mantenha um servidor PulseAudio ou
PipeWire-pulse ativo na sua sessão. O tarball contém `bin/`, `share/` e o modelo
`lib/systemd/user/org.babel.audio.service`, e pode ficar
em uma pasta do usuário, inclusive com espaços ou caracteres Unicode. Não exige
root nem contém script que copie arquivos para o sistema. A entrada `.desktop`
fornecida usa `Exec=babel-launch`; para instalá-la manualmente no menu, o diretório
`bin/` precisa estar no `PATH` da sessão gráfica e o ícone no tema do usuário.
Executar `bin/babel-launch` diretamente dispensa essa integração opcional.
O modelo de serviço no tarball não é instalado nem habilitado automaticamente;
se copiado manualmente para `~/.config/systemd/user`, seu `ExecStart` precisa
apontar para o caminho absoluto e devidamente escapado do launcher extraído.
O autostart do aplicativo usa a integração empacotada somente quando ela está
instalada e validada; para uma extração portátil, prefira o autostart XDG.

## Primeiro uso e configuração

O menu abre **a bandeja e o painel local**, conforme a CLI `babel-tray` real.
Selecione **Configurações** no ícone para abrir o endereço atual do painel.
O launcher passa `--port 0`; não fixa nem procura antecipadamente uma porta.
Se o desktop não oferecer bandeja, execute `babel --config CAMINHO_ABSOLUTO serve
--no-tray` em um terminal e abra o endereço que ele imprime.

O launcher usa `$XDG_CONFIG_HOME/babel/babel.toml` quando `XDG_CONFIG_HOME` é
absoluto, ou `$HOME/.config/babel/babel.toml`. Cria somente a pasta, com permissão
restrita para arquivos novos, e preserva configurações existentes. O aplicativo
salva a configuração ao aplicar os ajustes. O destino dos arquivos de sessão é
configurado separadamente; o padrão novo é a pasta absoluta `Babel` no diretório
pessoal. Os executáveis diretos aceitam `--config` para usar outra configuração.
Quando recebe argumentos explícitos, `babel-launch` os encaminha integralmente
a `babel-tray`, sem acrescentar configuração, porta ou subcomando. Por exemplo:
`babel-launch --config /home/ana/config/babel.toml --port 0`.

Depois de abrir, configure os dispositivos físicos em Roteamento e use a ação
de criação dos virtuais no painel. Originalmente não há uma sessão de tradução,
transcrição ou gravação em execução. O encaminhamento dos originais segue a
configuração e o uso dos dispositivos virtuais; fechar o aplicativo encerra-o.

Modelos, ambientes Python, whisper.cpp e chaves de nuvem não acompanham estes
pacotes. Os comandos de voz e os providers locais têm preparação opcional própria,
descrita em `docs/voice-commands.md` e `docs/other-providers.md` dentro da pasta de
documentação. O pacote não baixa modelos nem inicia serviços de IA na instalação.

## Remover

```sh
sudo apt remove babel-audio
# Fedora: sudo dnf remove babel-audio
# openSUSE: sudo zypper remove babel-audio
```

O gerenciador remove os arquivos empacotados; não modifica arquivos pessoais,
dispositivos do servidor de áudio nem preferências de início automático. Se
habilitou o início automático, desabilite-o no Babel antes de remover. Para uma
unidade habilitada manualmente, use antes `systemctl --user disable --now
org.babel.audio.service` e, depois da remoção, `systemctl --user daemon-reload`.
O instalador não remove drop-ins nem links criados pelo usuário. Encerre
o Babel e remova os dispositivos pelo painel se desejar. Para o tarball, encerre
o aplicativo e remova a pasta extraída e os atalhos que criou manualmente.

## Licença e metadados

O código do aplicativo e os arquivos de integração deste pacote têm licença MIT,
incluída em `copyright` na pasta de documentação. As dependências Rust conservam
suas licenças; `Cargo.lock` identifica suas versões. A fonte Manrope da interface
conserva sua licença SIL Open Font License, incluída em
`licenses/Manrope-OFL.txt` nessa mesma pasta. O contato de mantenedor do
pacote é um endereço `.invalid` deliberadamente não entregável, até que o projeto
publique um contato de distribuição. Não há credenciais, configurações pessoais,
modelos ou drivers de terceiros nos artefatos.
