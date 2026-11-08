import { recordingJournal, recordingRecovery } from './recording-storage.js';
// 前端MP4：单文件持续写入；Worker同步追加本地恢复副本，不缓存整段录像。
export function installRecording(button) {
  const source = () => document.querySelector(button.dataset.recordCanvas);
  const status = document.getElementById('status');
  const report = (text) => {
    const node = document.getElementById('message');
    if (node) node.textContent = text;
  };
  const ready = () =>
    status?.dataset.state === 'active' &&
    !document.body.classList.contains('login-required') &&
    source()?.width > 0 &&
    source()?.height > 0;
  const mp4Mime = (audio) =>
    globalThis.MediaRecorder &&
    ['avc1', 'avc1.640034']
      .flatMap((codec) =>
        audio
          ? ['mp4a.40.2', 'opus'].map((sound) => `video/mp4;codecs=${codec},${sound}`)
          : [`video/mp4;codecs=${codec}`],
      )
      .find((type) => MediaRecorder.isTypeSupported(type));
  const supported =
    !!mp4Mime(false) &&
    !!globalThis.showSaveFilePicker &&
    !!HTMLCanvasElement.prototype.captureStream &&
    !!navigator.storage?.getDirectory &&
    !!globalThis.Worker;
  const refreshRecovery = supported ? recordingRecovery(button, report) : async () => {};
  let choosing = false,
    session,
    generation = 0;
  const settings = { fps: 60, resolution: 'original', audio: true };
  const settingsButton = document.createElement('button');
  settingsButton.id = 'record-settings';
  settingsButton.textContent = '录屏设置';
  button.after(settingsButton);
  const audioAvailable = () => {
    const detail = { available: false };
    document.dispatchEvent(new CustomEvent('yoodesk-audio-capability', { detail }));
    return detail.available;
  };
  settingsButton.onclick = () => {
    const dialog = document.createElement('dialog');
    dialog.id = 'record-settings-dialog';
    dialog.innerHTML =
      '<form method="dialog"><h2>录屏设置</h2><label>分辨率 <select id="record-resolution"><option value="original">原始分辨率</option><option value="1080p">最高1080p</option><option value="720p">最高720p</option></select></label><label>帧率 <select id="record-fps"><option value="30">30 fps</option><option value="60">60 fps</option></select></label><label><input id="record-audio" type="checkbox">录制远端声音</label><p id="record-audio-help"></p><p>一次录制保存一个MP4。实际帧率受远端画面和本机编码性能影响。</p><button value="cancel">取消</button><button id="record-settings-save" value="save">保存设置</button></form>';
    document.body.append(dialog);
    const resolution = dialog.querySelector('#record-resolution'),
      fps = dialog.querySelector('#record-fps'),
      audio = dialog.querySelector('#record-audio');
    resolution.value = settings.resolution;
    fps.value = String(settings.fps);
    audio.checked = settings.audio;
    audio.disabled = !audioAvailable();
    dialog.querySelector('#record-audio-help').textContent = audio.disabled
      ? '当前远端没有可录制的音频能力，仅录制画面。'
      : '录制远端声音，不采集本机麦克风；关闭扬声器仍可录制声音。';
    dialog.onclose = () => {
      if (dialog.returnValue === 'save') {
        settings.resolution = resolution.value;
        settings.fps = Number(fps.value);
        settings.audio = audio.checked;
      }
      dialog.remove();
    };
    dialog.showModal();
  };
  function paintButton() {
    settingsButton.disabled = choosing || !!session;
    button.disabled = choosing || !!session?.stopping || (!session && (!supported || !ready()));
    button.dataset.recording = session
      ? session.stopping
        ? 'saving'
        : session.recorder.state === 'paused'
          ? 'paused'
          : 'recording'
      : 'idle';
    button.textContent = session
      ? session.stopping
        ? '正在保存…'
        : session.recorder.state === 'paused'
          ? '断线暂停 · 停止'
          : '停止录屏'
      : '录屏';
    button.setAttribute('aria-pressed', String(!!session));
    button.title = supported
      ? `单文件MP4，${settings.resolution === 'original' ? '原始分辨率' : settings.resolution}/${settings.fps}fps；可在录屏设置选择画面与远端声音`
      : '需要H.264 MP4和本地文件写入支持，请使用新版Chrome或Edge';
  }
  const stop = async () => {
    generation++;
    if (!session) return;
    const current = session;
    if (!current.stopping) {
      current.stopping = true;
      clearInterval(current.timer);
      if (current.recorder.state !== 'inactive') current.recorder.stop();
      paintButton();
    }
    await current.done;
  };
  button.onclick = async () => {
    if (session) {
      await stop();
      return;
    }
    if (choosing || !supported || !ready()) return;
    choosing = true;
    const attempt = ++generation;
    paintButton();
    let writer, stream, journal, capturedAudio;
    const journalName = `${button.dataset.recordName}-${new Date().toISOString().replace(/[:.]/g, '-')}-${crypto.randomUUID().slice(0, 8)}.mp4`;
    try {
      const wantsAudio = settings.audio && audioAvailable();
      const mime = mp4Mime(wantsAudio);
      if (!mime) throw new Error('当前浏览器不支持所选MP4音视频编码组合');
      if (wantsAudio) {
        const detail = {};
        document.dispatchEvent(new CustomEvent('yoodesk-recording-audio', { detail }));
        capturedAudio = detail.audio;
        if (!capturedAudio) throw new Error('远端声音尚未就绪');
      }
      const picking = showSaveFilePicker({
        suggestedName: `${button.dataset.recordName}-${new Date().toISOString().replace(/[:.]/g, '-')}.mp4`,
        id: 'yoodesk-recording',
        excludeAcceptAllOption: true,
        types: [{ description: 'MP4 视频', accept: { 'video/mp4': ['.mp4'] } }],
      });
      // 两个API都在同一次点击中启动，避免文件选择器或WebAudio丢失用户激活。
      const [handle] = await Promise.all([picking, capturedAudio?.ready]);
      if (!ready() || attempt !== generation) return;
      writer = await handle.createWritable();
      if (!ready() || attempt !== generation) {
        await writer.abort();
        return;
      }
      journal = recordingJournal();
      await journal.request('open', { name: journalName });
      if (!ready() || attempt !== generation) {
        await writer.abort();
        await journal.request('remove', { name: journalName });
        journal.terminate();
        return;
      }
      const input = source(),
        output = document.createElement('canvas');
      // 编码尺寸全程固定；远端切分辨率/显示器时等比居中，避免拉伸和容器变化。
      const [limitWidth, limitHeight] =
        settings.resolution === '720p'
          ? [1280, 720]
          : settings.resolution === '1080p'
            ? [1920, 1080]
            : [input.width, input.height];
      const scale = Math.min(1, limitWidth / input.width, limitHeight / input.height);
      const fps = settings.fps;
      output.width = Math.max(2, Math.floor((input.width * scale) / 2) * 2);
      output.height = Math.max(2, Math.floor((input.height * scale) / 2) * 2);
      const context = output.getContext('2d', { alpha: false });
      const draw = () => {
        const image = source();
        if (!image?.width || !image?.height || !ready()) return;
        const ratio = Math.min(output.width / image.width, output.height / image.height);
        const width = image.width * ratio,
          height = image.height * ratio;
        context.fillStyle = '#000';
        context.fillRect(0, 0, output.width, output.height);
        context.drawImage(
          image,
          (output.width - width) / 2,
          (output.height - height) / 2,
          width,
          height,
        );
      };
      draw();
      stream = output.captureStream(fps);
      if (capturedAudio) {
        const track = capturedAudio.stream.getAudioTracks()[0];
        if (!track || track.readyState !== 'live') throw new Error('远端录制音轨已结束');
        stream.addTrack(track.clone());
      }
      const videoBitsPerSecond = Math.min(
        40_000_000,
        Math.max(2_000_000, Math.round(output.width * output.height * fps * 0.1)),
      );
      const recorder = new MediaRecorder(stream, {
        mimeType: mime,
        videoBitsPerSecond,
        audioBitsPerSecond: 192_000,
      });
      let resolve,
        tail = Promise.resolve(),
        bytes = 0,
        pendingBytes = 0,
        writeError;
      const current = {
        recorder,
        stopping: false,
        done: new Promise((complete) => {
          resolve = complete;
        }),
      };
      session = current;
      const fail = (error) => {
        writeError = error;
        report(`录屏失败：${error.message}。已落盘画面可通过“恢复录屏”导出。`);
        void stop();
      };
      recorder.ondataavailable = (event) => {
        if (!event.data.size || writeError) return;
        pendingBytes += event.data.size;
        if (pendingBytes > 16 * 1024 * 1024) {
          fail(new Error('磁盘写入过慢，待写数据超过16MiB'));
          return;
        }
        tail = tail
          .then(async () => {
            if (writeError) return;
            const saved = await journal.request('write', { bytes: await event.data.arrayBuffer() });
            button.dataset.savedBytes = String(saved);
            await writer.write(event.data);
            bytes += event.data.size;
          })
          .catch(fail)
          .finally(() => {
            pendingBytes -= event.data.size;
          });
      };
      recorder.onerror = (event) => fail(event.error || new Error('MP4编码器异常'));
      recorder.onstop = async () => {
        current.stopping = true;
        clearInterval(current.timer);
        stream.getTracks().forEach((track) => track.stop());
        paintButton();
        try {
          await tail;
          if (writeError) {
            if (writer) await writer.abort();
          } else {
            if (writer) await writer.close();
            writer = null;
            if (!bytes) throw new Error('编码器未输出MP4数据');
            await journal.request('remove', { name: journalName });
            button.dataset.savedBytes = String(bytes);
            report(`录屏已保存：${handle.name}（单个MP4文件）`);
          }
        } catch (error) {
          report(`录像保存失败：${error.message}。请检查磁盘空间及写入权限。`);
        } finally {
          if (writer) await writer.abort().catch(() => {});
          await journal
            .request('close')
            .catch((error) => report(`本地录像恢复副本关闭失败：${error.message}`));
          journal.terminate();
          writer = null;
          session = null;
          capturedAudio?.release();
          resolve();
          paintButton();
          void refreshRecovery();
        }
      };
      delete button.dataset.savedBytes;
      recorder.start(1000);
      current.timer = setInterval(() => {
        try {
          if (recorder.state === 'recording') draw();
        } catch (error) {
          fail(error);
        }
      }, 1000 / fps);
      report(
        `录屏已开始：${output.width}×${output.height} / ${fps}fps，${capturedAudio ? '画面＋远端声音' : '仅画面'}，单个MP4。异常关闭后可在同一浏览器恢复已落盘内容。`,
      );
    } catch (error) {
      stream?.getTracks().forEach((track) => track.stop());
      if (writer) await writer.abort().catch(() => {});
      journal?.terminate();
      session = null;
      if (error.name !== 'AbortError') report(`无法录屏：${error.message}`);
    } finally {
      if (!session) capturedAudio?.release();
      choosing = false;
      paintButton();
    }
  };
  const changed = () => {
    if (document.body.classList.contains('login-required')) void stop();
    else if (session && !session.stopping) {
      if (!ready() && session.recorder.state === 'recording') {
        session.recorder.requestData();
        session.recorder.pause();
      } else if (ready() && session.recorder.state === 'paused') session.recorder.resume();
    }
    paintButton();
  };
  new MutationObserver(changed).observe(status, {
    attributes: true,
    attributeFilter: ['data-state'],
  });
  new MutationObserver(changed).observe(document.body, {
    attributes: true,
    attributeFilter: ['class'],
  });
  document.addEventListener('yoodesk-stop-recording', (event) => event.detail.push(stop()));
  window.addEventListener('beforeunload', (event) => {
    if (session || choosing) {
      event.preventDefault();
      event.returnValue = '';
    }
  });
  paintButton();
  void refreshRecovery();
}

for (const button of document.querySelectorAll('[data-record-canvas]')) installRecording(button);
