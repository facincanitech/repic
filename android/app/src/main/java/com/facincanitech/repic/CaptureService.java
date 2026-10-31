package com.facincanitech.repic;

import android.Manifest;
import android.annotation.SuppressLint;
import android.app.Activity;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.ServiceInfo;
import android.graphics.Color;
import android.graphics.PixelFormat;
import android.graphics.drawable.Drawable;
import android.graphics.drawable.GradientDrawable;
import android.graphics.drawable.LayerDrawable;
import android.hardware.display.DisplayManager;
import android.hardware.display.VirtualDisplay;
import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioPlaybackCaptureConfiguration;
import android.media.AudioRecord;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaCodecList;
import android.media.MediaFormat;
import android.media.MediaMuxer;
import android.media.MediaRecorder;
import android.media.projection.MediaProjection;
import android.media.projection.MediaProjectionManager;
import android.net.Uri;
import android.os.Bundle;
import android.os.Environment;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.IBinder;
import android.os.Looper;
import android.os.ParcelFileDescriptor;
import android.os.StatFs;
import android.provider.MediaStore;
import android.provider.Settings;
import android.text.SpannableString;
import android.text.style.RelativeSizeSpan;
import android.util.DisplayMetrics;
import android.util.Log;
import android.view.Display;
import android.view.Gravity;
import android.view.MotionEvent;
import android.view.Surface;
import android.view.View;
import android.view.ViewConfiguration;
import android.view.WindowManager;
import android.widget.LinearLayout;
import android.widget.TextView;
import android.widget.Toast;
import java.io.File;
import java.io.FileWriter;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.text.SimpleDateFormat;
import java.util.ArrayList;
import java.util.Date;
import java.util.List;
import java.util.Locale;

/**
 * Grava a tela em loop, guardando em disco o vídeo e o som já codificados.
 * Clipar monta um MP4 com o passado; gravar marca o começo e monta ao parar.
 */
public class CaptureService extends Service {

    static final String ACTION_START = "com.facincanitech.repic.START";
    static final String ACTION_CLIP = "com.facincanitech.repic.CLIP";
    static final String ACTION_RECORD = "com.facincanitech.repic.RECORD";
    static final String ACTION_STOP = "com.facincanitech.repic.STOP";
    static final String ACTION_RECONFIGURE = "com.facincanitech.repic.RECONFIGURE";
    static final String EXTRA_RESULT = "result";
    static final String EXTRA_DATA = "data";

    private static final String TAG = "RePic";
    private static final String CHANNEL = "capture";
    private static final int NOTIFICATION_ID = 1;
    private static final int SAMPLE_RATE = 48_000;
    private static final int CHANNELS = 2;
    private static final int AUDIO_BITRATE = 192_000;
    /** Quadro-chave a cada 2s: um clipe começa até 2s antes do pedido, nunca depois. */
    private static final int KEYFRAME_SECONDS = 2;
    private static final long EXTRA_KEEP_US = 20_000_000L;
    /** Tempo pro codificador entregar os últimos quadros antes de montar o MP4. */
    private static final long ENCODER_DRAIN_MS = 400;
    private static final long DISK_RESERVE_BYTES = 500_000_000L;
    /** Som AAC em bytes por segundo, pra estimar o tamanho. */
    private static final long AUDIO_BYTES_PER_SEC = AUDIO_BITRATE / 8;

    private static volatile CaptureService instance;

    static CaptureService running() {
        return instance;
    }

    private final Handler main = new Handler(Looper.getMainLooper());
    private HandlerThread controlThread;
    private Handler control;

    private RePicConfig config;
    private MediaProjection projection;
    private VirtualDisplay virtualDisplay;
    private DisplayManager.DisplayListener displayListener;

    private MediaCodec videoCodec;
    private Surface videoSurface;
    private volatile MediaFormat videoFormat;
    private volatile MediaFormat audioFormat;
    private Thread videoThread;
    private int encoderFailures;
    private static final long ENCODER_RETRY_MS = 3000;
    /** A configuração atual já gerou vídeo: se cair agora, a culpa não é dela. */
    private volatile boolean videoProved;
    private Thread audioThread;
    private volatile boolean videoRunning;
    private volatile boolean audioRunning;
    private int videoWidth;
    private int videoHeight;

    private SampleRing videoRing;
    private SampleRing audioRing;

    private volatile boolean recording;
    private volatile boolean saving;
    private long recStartUs;
    private long recStartRealtime;
    private long recStartVideoBytes;
    private long recStartAudioBytes;
    private volatile long recBytes;
    private int ticks;

    private View bubble;
    private TextView bubbleRound;
    private LinearLayout bubblePanel;
    private WindowManager.LayoutParams bubbleParams;
    private int bubbleHomeX;
    private int bubbleHomeY;
    private TextView bubbleStatus;
    private TextView bubbleRecord;
    private volatile String lastMessage = "";

    // --- Estado pra interface -------------------------------------------

    boolean isRecording() {
        return recording;
    }

    boolean isSaving() {
        return saving;
    }

    long recordingSeconds() {
        return recording ? (android.os.SystemClock.elapsedRealtime() - recStartRealtime) / 1000 : 0;
    }

    long recordingBytes() {
        return recBytes;
    }

    String lastMessage() {
        return lastMessage;
    }

    String videoSize() {
        return videoWidth + "x" + videoHeight;
    }

    // --- Ciclo de vida --------------------------------------------------

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    @Override
    public void onCreate() {
        super.onCreate();
        controlThread = new HandlerThread("repic-control");
        controlThread.start();
        control = new Handler(controlThread.getLooper());
        createChannel();
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String action = intent == null ? null : intent.getAction();
        if (ACTION_START.equals(action)) {
            // Precisa estar em primeiro plano antes de pedir a projeção (Android 14+).
            startForegroundNow();
            int result = intent.getIntExtra(EXTRA_RESULT, Activity.RESULT_CANCELED);
            Intent data = intent.getParcelableExtra(EXTRA_DATA);
            control.post(() -> startCapture(result, data));
        } else if (ACTION_CLIP.equals(action)) {
            control.post(this::saveClip);
        } else if (ACTION_RECORD.equals(action)) {
            control.post(this::toggleRecording);
        } else if (ACTION_RECONFIGURE.equals(action)) {
            control.post(this::reconfigure);
        } else if (ACTION_STOP.equals(action)) {
            control.post(this::stopEverything);
        } else if (instance == null) {
            // O sistema recriou o serviço sem a permissão de captura: não há o que fazer.
            stopSelf();
        }
        return START_NOT_STICKY;
    }

    @Override
    public void onDestroy() {
        instance = null;
        if (controlThread != null) {
            controlThread.quitSafely();
        }
        super.onDestroy();
    }

    private void startCapture(int result, Intent data) {
        try {
            config = RePicConfig.load(this);
            File base = new File(getFilesDir(), "buffer");
            videoRing = new SampleRing(new File(base, "video"));
            audioRing = new SampleRing(new File(base, "audio"));

            MediaProjectionManager manager = getSystemService(MediaProjectionManager.class);
            projection = manager.getMediaProjection(result, data);
            if (projection == null) {
                throw new IllegalStateException("sem permissão de captura");
            }
            // Obrigatório antes de criar a tela virtual (Android 14+).
            projection.registerCallback(
                new MediaProjection.Callback() {
                    @Override
                    public void onStop() {
                        // O sistema ou a pessoa encerrou a captura.
                        control.post(CaptureService.this::stopEverything);
                    }
                },
                control
            );
            startVideo();
            startAudio();
            listenRotation();
            instance = this;
            main.post(this::updateBubbleVisibility);
            control.postDelayed(this::tick, 1000);
            log("captura: começou " + videoSize());
        } catch (Exception e) {
            log("captura: falhou: " + e);
            message(getString(R.string.capture_failed));
            stopEverything();
        }
    }

    private void stopEverything() {
        if (recording) {
            finishRecording();
        }
        instance = null;
        control.removeCallbacksAndMessages(null);
        stopAudio();
        stopVideo();
        if (displayListener != null) {
            getSystemService(DisplayManager.class).unregisterDisplayListener(displayListener);
            displayListener = null;
        }
        if (virtualDisplay != null) {
            virtualDisplay.release();
            virtualDisplay = null;
        }
        if (projection != null) {
            projection.stop();
            projection = null;
        }
        if (videoRing != null) {
            videoRing.clear();
        }
        if (audioRing != null) {
            audioRing.clear();
        }
        main.post(() -> {
            removeBubble();
            stopForeground(STOP_FOREGROUND_REMOVE);
            stopSelf();
        });
        log("captura: parou");
    }

    private void reconfigure() {
        RePicConfig updated = RePicConfig.load(this);
        boolean sameVideo = config != null && config.sameVideo(updated);
        boolean sameAudio = config != null && config.audio == updated.audio && config.mic == updated.mic;
        config = updated;
        main.post(this::updateBubbleVisibility);
        if (projection == null || (sameVideo && sameAudio)) {
            return;
        }
        if (recording) {
            finishRecording();
        }
        if (!sameAudio) {
            stopAudio();
            audioRing.clear();
            startAudio();
        }
        if (!sameVideo) {
            videoProved = false;
            encoderFailures = 0;
            restartVideo();
        }
    }

    /** A cada segundo: limpa o que venceu, atualiza o medidor e confere os limites. */
    private void tick() {
        if (projection == null) {
            return;
        }
        // Tela parada quase não gera quadro-chave, e um clipe só começa em um:
        // sem isso ele voltaria bem mais que o pedido.
        if (++ticks % KEYFRAME_SECONDS == 0) {
            requestKeyframe();
        }
        long now = nowUs();
        long keep = (config.seconds * 1_000_000L) + EXTRA_KEEP_US;
        videoRing.prune(now - keep);
        audioRing.prune(now - keep);
        if (recording) {
            long video = videoRing.totalBytes() - recStartVideoBytes;
            long audio = audioRing.totalBytes() - recStartAudioBytes;
            recBytes = video + audio;
            long secs = Math.max(1, recordingSeconds());
            // Parar ainda inclui o que está no codificador: dois segundos de folga.
            long margin = 2 * recBytes / secs;
            if (config.limitMb > 0 && recBytes + margin >= config.limitMb * 1_000_000L) {
                log("gravação: chegou no limite");
                message(getString(R.string.limit_reached));
                finishRecording();
            } else if (freeBytes() < 2 * recBytes + DISK_RESERVE_BYTES) {
                log("gravação: disco quase cheio");
                message(getString(R.string.disk_full));
                finishRecording();
            }
        }
        main.post(this::updateBubble);
        control.postDelayed(this::tick, 1000);
    }

    private void requestKeyframe() {
        MediaCodec codec = videoCodec;
        if (codec != null) {
            try {
                Bundle params = new Bundle();
                params.putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0);
                codec.setParameters(params);
            } catch (IllegalStateException ignored) {
                // Codificador reiniciando: o próximo pedido resolve.
            }
        }
    }

    private long freeBytes() {
        StatFs stat = new StatFs(getFilesDir().getPath());
        return stat.getAvailableBytes();
    }

    private static long nowUs() {
        return System.nanoTime() / 1000;
    }

    // --- Vídeo ----------------------------------------------------------

    private void startVideo() throws IOException {
        DisplayMetrics metrics = getResources().getDisplayMetrics();
        int[] real = realSize();
        // Depois de falhas do codificador, vale o degrau de baixo: ver encoderFailed.
        int target = encoderFailures >= 3 && (config.resolution == 0 || config.resolution > 720) ? 720 : config.resolution;
        int[] size = fitSize(real[0], real[1], target);
        int refresh = displayRefresh();
        int fps = config.fps == 0 ? refresh : Math.min(config.fps, refresh);
        if (encoderFailures >= 1) {
            fps = Math.min(fps, encoderFailures == 1 ? 60 : 30);
        }

        MediaCodec codec = null;
        Exception lastError = null;
        // Nem todo codificador aceita a tela nativa: vai reduzindo até um aceitar.
        for (int[] candidate : sizeCandidates(size)) {
            try {
                codec = configureEncoder(candidate[0], candidate[1], fps);
                videoWidth = candidate[0];
                videoHeight = candidate[1];
                break;
            } catch (Exception e) {
                lastError = e;
                if (codec != null) {
                    codec.release();
                    codec = null;
                }
            }
        }
        if (codec == null) {
            throw new IOException("nenhum tamanho de vídeo aceito: " + lastError);
        }
        videoCodec = codec;
        videoSurface = codec.createInputSurface();
        codec.start();
        if (virtualDisplay == null) {
            virtualDisplay = projection.createVirtualDisplay(
                "RePic",
                videoWidth,
                videoHeight,
                metrics.densityDpi,
                DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
                videoSurface,
                null,
                control
            );
        } else {
            virtualDisplay.resize(videoWidth, videoHeight, metrics.densityDpi);
            virtualDisplay.setSurface(videoSurface);
        }
        videoRunning = true;
        final MediaCodec running = codec;
        videoThread = new Thread(() -> drainVideo(running), "repic-video");
        videoThread.start();
    }

    private MediaCodec configureEncoder(int width, int height, int fps) throws IOException {
        MediaFormat format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, width, height);
        format.setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface);
        format.setInteger(MediaFormat.KEY_BIT_RATE, config.bitrate(width, height, fps));
        format.setInteger(MediaFormat.KEY_FRAME_RATE, fps);
        format.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, KEYFRAME_SECONDS);
        // Tela parada não gera quadros: repete pro vídeo não travar nem perder o tempo.
        format.setLong(MediaFormat.KEY_REPEAT_PREVIOUS_FRAME_AFTER, 1_000_000L / Math.max(fps, 1));
        // A tela manda até 120 quadros por segundo: sem isso o FPS escolhido é ignorado.
        format.setFloat(MediaFormat.KEY_MAX_FPS_TO_ENCODER, fps);
        MediaCodec codec = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC);
        try {
            codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
        } catch (RuntimeException e) {
            codec.release();
            throw e;
        }
        return codec;
    }

    private void drainVideo(MediaCodec codec) {
        MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
        while (videoRunning) {
            try {
                int index = codec.dequeueOutputBuffer(info, 50_000);
                if (index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                    videoFormat = codec.getOutputFormat();
                } else if (index >= 0) {
                    ByteBuffer data = codec.getOutputBuffer(index);
                    boolean config = (info.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0;
                    if (data != null && info.size > 0 && !config) {
                        data.position(info.offset);
                        data.limit(info.offset + info.size);
                        videoRing.append(data, info.presentationTimeUs, info.flags);
                        videoProved = true;
                    }
                    codec.releaseOutputBuffer(index, false);
                }
            } catch (IOException e) {
                log("vídeo: não consegui gravar no disco: " + e);
                message(getString(R.string.disk_full));
                sleep(1000);
            } catch (IllegalStateException e) {
                if (videoRunning) {
                    log("vídeo: codificador caiu: " + e);
                    control.post(this::encoderFailed);
                }
                return;
            }
        }
    }

    private void stopVideo() {
        videoRunning = false;
        join(videoThread);
        videoThread = null;
        if (videoCodec != null) {
            try {
                videoCodec.stop();
            } catch (IllegalStateException ignored) {
                // Já tinha parado.
            }
            videoCodec.release();
            videoCodec = null;
        }
        if (videoSurface != null) {
            videoSurface.release();
            videoSurface = null;
        }
        videoFormat = null;
    }

    /**
     * Tem chip que aceita a configuração e só recusa no primeiro quadro (A52s:
     * 1080x2400 a 120 fps). Desce um degrau: 60 fps, 30 fps, 720p e desiste.
     */
    private void encoderFailed() {
        if (projection == null) {
            return;
        }
        if (recording) {
            finishRecording();
        }
        if (videoProved) {
            // Essa configuração já funcionou: outro app pegou o codificador (o player,
            // por exemplo). Baixar a qualidade não resolve, então espera ele soltar.
            control.postDelayed(this::retryVideo, ENCODER_RETRY_MS);
            return;
        }
        if (++encoderFailures > 3) {
            log("vídeo: desisti depois de " + encoderFailures + " falhas");
            message(getString(R.string.capture_failed));
            stopEverything();
            return;
        }
        restartVideo();
    }

    private void retryVideo() {
        if (projection == null) {
            return;
        }
        if (recording) {
            finishRecording();
        }
        restartVideo();
    }

    private void restartVideo() {
        try {
            // A tela virtual fica sem destino por um instante.
            virtualDisplay.setSurface(null);
            stopVideo();
            videoRing.clear();
            startVideo();
            log("vídeo: recomeçou " + videoSize() + ", falhas: " + encoderFailures);
        } catch (Exception e) {
            log("vídeo: não recomeçou: " + e);
            if (videoProved) {
                control.postDelayed(this::retryVideo, ENCODER_RETRY_MS);
                return;
            }
            message(getString(R.string.capture_failed));
            stopEverything();
        }
    }

    /** Girar a tela muda o tamanho do vídeo: um MP4 só tem um, então recomeça. */
    private void listenRotation() {
        DisplayManager displays = getSystemService(DisplayManager.class);
        displayListener = new DisplayManager.DisplayListener() {
            @Override
            public void onDisplayAdded(int id) {}

            @Override
            public void onDisplayRemoved(int id) {}

            @Override
            public void onDisplayChanged(int id) {
                if (id != Display.DEFAULT_DISPLAY || projection == null) {
                    return;
                }
                int[] real = realSize();
                boolean landscapeNow = real[0] > real[1];
                if (landscapeNow != (videoWidth > videoHeight)) {
                    log("tela girou");
                    if (recording) {
                        finishRecording();
                    }
                    restartVideo();
                }
            }
        };
        displays.registerDisplayListener(displayListener, control);
    }

    @SuppressWarnings("deprecation")
    private int[] realSize() {
        WindowManager wm = getSystemService(WindowManager.class);
        DisplayMetrics metrics = new DisplayMetrics();
        wm.getDefaultDisplay().getRealMetrics(metrics);
        return new int[] { metrics.widthPixels, metrics.heightPixels };
    }

    @SuppressWarnings("deprecation")
    private int displayRefresh() {
        WindowManager wm = getSystemService(WindowManager.class);
        return Math.max(24, Math.round(wm.getDefaultDisplay().getRefreshRate()));
    }

    /** Reduz pelo menor lado, mantendo a proporção; medidas pares, que o H.264 exige. */
    static int[] fitSize(int width, int height, int target) {
        int shortSide = Math.min(width, height);
        double scale = (target > 0 && target < shortSide) ? (double) target / shortSide : 1.0;
        return new int[] { even(width * scale), even(height * scale) };
    }

    private static int even(double value) {
        return ((int) Math.round(value)) / 2 * 2;
    }

    /** O tamanho pedido e opções menores, alinhadas como os codificadores preferem. */
    private static List<int[]> sizeCandidates(int[] size) {
        List<int[]> out = new ArrayList<>();
        MediaCodecInfo.VideoCapabilities caps = avcCapabilities();
        for (double scale : new double[] { 1.0, 0.75, 0.5, 0.33 }) {
            for (int align : new int[] { 2, 16 }) {
                int w = (int) (size[0] * scale) / align * align;
                int h = (int) (size[1] * scale) / align * align;
                if (w >= 16 && h >= 16 && (caps == null || caps.isSizeSupported(w, h))) {
                    out.add(new int[] { w, h });
                }
            }
        }
        return out;
    }

    private static MediaCodecInfo.VideoCapabilities avcCapabilities() {
        for (MediaCodecInfo info : new MediaCodecList(MediaCodecList.REGULAR_CODECS).getCodecInfos()) {
            if (!info.isEncoder()) {
                continue;
            }
            for (String type : info.getSupportedTypes()) {
                if (type.equalsIgnoreCase(MediaFormat.MIMETYPE_VIDEO_AVC)) {
                    return info.getCapabilitiesForType(type).getVideoCapabilities();
                }
            }
        }
        return null;
    }

    // --- Som ------------------------------------------------------------

    private void startAudio() {
        if (!config.audio && !config.mic) {
            return;
        }
        audioRunning = true;
        audioThread = new Thread(this::runAudio, "repic-audio");
        audioThread.start();
    }

    private void stopAudio() {
        audioRunning = false;
        join(audioThread);
        audioThread = null;
        audioFormat = null;
    }

    @SuppressLint("MissingPermission")
    private AudioRecord openPlaybackCapture(int bufferBytes) {
        try {
            AudioPlaybackCaptureConfiguration capture = new AudioPlaybackCaptureConfiguration.Builder(projection)
                .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
                .addMatchingUsage(AudioAttributes.USAGE_GAME)
                .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
                .build();
            AudioRecord record = new AudioRecord.Builder()
                .setAudioFormat(pcmFormat())
                .setBufferSizeInBytes(bufferBytes)
                .setAudioPlaybackCaptureConfig(capture)
                .build();
            return ready(record, "som do aparelho");
        } catch (Exception e) {
            log("som do aparelho: não abriu: " + e);
            return null;
        }
    }

    @SuppressLint("MissingPermission")
    private AudioRecord openMic(int bufferBytes) {
        try {
            AudioRecord record = new AudioRecord.Builder()
                .setAudioSource(MediaRecorder.AudioSource.MIC)
                .setAudioFormat(pcmFormat())
                .setBufferSizeInBytes(bufferBytes)
                .build();
            return ready(record, "microfone");
        } catch (Exception e) {
            log("microfone: não abriu: " + e);
            return null;
        }
    }

    private AudioRecord ready(AudioRecord record, String name) {
        if (record.getState() != AudioRecord.STATE_INITIALIZED) {
            log(name + ": não inicializou");
            record.release();
            return null;
        }
        record.startRecording();
        log(name + ": capturando");
        return record;
    }

    private static AudioFormat pcmFormat() {
        return new AudioFormat.Builder()
            .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
            .setSampleRate(SAMPLE_RATE)
            .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
            .build();
    }

    private void runAudio() {
        boolean hasPermission =
            checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED;
        if (!hasPermission) {
            log("áudio: sem a permissão de gravar som");
        }
        int minBuffer = AudioRecord.getMinBufferSize(
            SAMPLE_RATE,
            AudioFormat.CHANNEL_IN_STEREO,
            AudioFormat.ENCODING_PCM_16BIT
        );
        int bufferBytes = Math.max(minBuffer * 4, SAMPLE_RATE * CHANNELS * 2 / 2);
        AudioRecord playback = hasPermission && config.audio ? openPlaybackCapture(bufferBytes) : null;
        AudioRecord mic = hasPermission && config.mic ? openMic(bufferBytes) : null;
        MediaCodec encoder = null;
        try {
            MediaFormat format = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_AAC, SAMPLE_RATE, CHANNELS);
            format.setInteger(MediaFormat.KEY_AAC_PROFILE, MediaCodecInfo.CodecProfileLevel.AACObjectLC);
            format.setInteger(MediaFormat.KEY_BIT_RATE, AUDIO_BITRATE);
            format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, 16 * 1024);
            encoder = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_AUDIO_AAC);
            encoder.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
            encoder.start();
            pumpAudio(encoder, playback, mic);
        } catch (Exception e) {
            log("áudio: falhou: " + e);
        } finally {
            release(playback);
            release(mic);
            if (encoder != null) {
                try {
                    encoder.stop();
                } catch (IllegalStateException ignored) {
                    // Já tinha parado.
                }
                encoder.release();
            }
        }
    }

    /**
     * Lê o som, mistura com o microfone e codifica. Sempre sai uma faixa contínua:
     * sem nenhuma fonte, ela é silêncio, igual no Windows.
     */
    private void pumpAudio(MediaCodec encoder, AudioRecord playback, AudioRecord mic) throws IOException {
        int chunkShorts = SAMPLE_RATE / 50 * CHANNELS; // 20 ms
        short[] pcm = new short[chunkShorts];
        short[] other = new short[chunkShorts];
        MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
        long anchorUs = 0;
        long frames = 0;
        long chunkUs = chunkShorts / CHANNELS * 1_000_000L / SAMPLE_RATE;
        while (audioRunning) {
            int read;
            if (playback != null) {
                read = playback.read(pcm, 0, chunkShorts, AudioRecord.READ_BLOCKING);
            } else if (mic != null) {
                read = mic.read(pcm, 0, chunkShorts, AudioRecord.READ_BLOCKING);
            } else {
                sleep(chunkUs / 1000);
                java.util.Arrays.fill(pcm, (short) 0);
                read = chunkShorts;
            }
            if (read <= 0) {
                sleep(5);
                continue;
            }
            if (playback != null && mic != null) {
                int got = mic.read(other, 0, read, AudioRecord.READ_NON_BLOCKING);
                for (int i = 0; i < got; i++) {
                    int sum = pcm[i] + other[i];
                    pcm[i] = (short) Math.max(Short.MIN_VALUE, Math.min(Short.MAX_VALUE, sum));
                }
            }
            int readFrames = read / CHANNELS;
            long now = nowUs();
            long startOfChunk = now - readFrames * 1_000_000L / SAMPLE_RATE;
            long expected = anchorUs + frames * 1_000_000L / SAMPLE_RATE;
            // Conta amostras pra não ter buraco; o relógio só corrige desvio grande.
            if (anchorUs == 0 || startOfChunk - expected > 100_000L) {
                anchorUs = startOfChunk;
                frames = 0;
                expected = startOfChunk;
            }
            queuePcm(encoder, pcm, read, expected);
            frames += readFrames;
            drainAudio(encoder, info);
        }
    }

    private void queuePcm(MediaCodec encoder, short[] pcm, int count, long ptsUs) {
        int offset = 0;
        while (offset < count && audioRunning) {
            int index = encoder.dequeueInputBuffer(10_000);
            if (index < 0) {
                return; // codificador atrasado: perde esse pedacinho, sem travar a leitura
            }
            ByteBuffer input = encoder.getInputBuffer(index);
            if (input == null) {
                return;
            }
            input.clear();
            // PCM do Android é little-endian; o padrão do ByteBuffer viraria ruído.
            input.order(ByteOrder.LITTLE_ENDIAN);
            int n = Math.min(count - offset, input.remaining() / 2);
            for (int i = 0; i < n; i++) {
                input.putShort(pcm[offset + i]);
            }
            long pts = ptsUs + (offset / CHANNELS) * 1_000_000L / SAMPLE_RATE;
            encoder.queueInputBuffer(index, 0, n * 2, pts, 0);
            offset += n;
        }
    }

    private void drainAudio(MediaCodec encoder, MediaCodec.BufferInfo info) throws IOException {
        while (true) {
            int index = encoder.dequeueOutputBuffer(info, 0);
            if (index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                audioFormat = encoder.getOutputFormat();
            } else if (index >= 0) {
                ByteBuffer data = encoder.getOutputBuffer(index);
                boolean codecConfig = (info.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0;
                if (data != null && info.size > 0 && !codecConfig) {
                    data.position(info.offset);
                    data.limit(info.offset + info.size);
                    audioRing.append(data, info.presentationTimeUs, info.flags);
                }
                encoder.releaseOutputBuffer(index, false);
            } else {
                return;
            }
        }
    }

    private static void release(AudioRecord record) {
        if (record != null) {
            try {
                record.stop();
            } catch (IllegalStateException ignored) {
                // Já tinha parado.
            }
            record.release();
        }
    }

    // --- Salvar ---------------------------------------------------------

    private void saveClip() {
        if (projection == null) {
            return;
        }
        if (saving) {
            log("clipe: já está salvando outro, ignorado");
            return;
        }
        saving = true;
        long toUs = nowUs();
        long fromUs = toUs - config.seconds * 1_000_000L;
        videoRing.hold();
        audioRing.hold();
        message(getString(R.string.clipping));
        sleep(ENCODER_DRAIN_MS);
        // Fora da thread de controle: um clipe longo não pode travar o medidor.
        new Thread(() -> {
            try {
                Uri uri = writeMp4(fromUs, toUs);
                log("clipe: salvo " + uri);
                message(getString(R.string.clip_saved));
            } catch (Exception e) {
                log("clipe: falhou: " + e);
                message(getString(R.string.save_failed));
            } finally {
                videoRing.release();
                audioRing.release();
                saving = false;
            }
        }, "repic-clip").start();
    }

    private void toggleRecording() {
        if (projection == null) {
            return;
        }
        if (recording) {
            finishRecording();
            return;
        }
        // A gravação começa num quadro-chave: pede um pra ser agora.
        requestKeyframe();
        recStartUs = nowUs();
        recStartRealtime = android.os.SystemClock.elapsedRealtime();
        recStartVideoBytes = videoRing.totalBytes();
        recStartAudioBytes = audioRing.totalBytes();
        recBytes = 0;
        videoRing.hold();
        audioRing.hold();
        recording = true;
        log("gravação: começou");
        message(getString(R.string.recording));
        main.post(this::updateBubble);
    }

    private void finishRecording() {
        recording = false;
        long fromUs = recStartUs;
        long toUs = nowUs();
        message(getString(R.string.saving_recording));
        sleep(ENCODER_DRAIN_MS);
        try {
            while (saving) {
                sleep(100);
            }
            saving = true;
            Uri uri = writeMp4(fromUs, toUs);
            log("gravação: salva " + uri);
            message(getString(R.string.recording_saved));
        } catch (Exception e) {
            log("gravação: falhou: " + e);
            message(getString(R.string.save_failed));
        } finally {
            saving = false;
            videoRing.release();
            audioRing.release();
            main.post(this::updateBubble);
        }
    }

    /** Monta um MP4 em Movies/RePic com as amostras da janela de tempo. */
    private Uri writeMp4(long fromUs, long toUs) throws IOException {
        MediaFormat video = videoFormat;
        if (video == null) {
            throw new IOException("o vídeo ainda não começou");
        }
        List<SampleRing.Sample> videoSamples = videoRing.snapshot(fromUs, toUs, true);
        if (videoSamples.isEmpty()) {
            throw new IOException("nada gravado ainda");
        }
        // Começa no primeiro quadro completo e o som acompanha a partir dali.
        long baseUs = videoSamples.get(0).ptsUs;
        MediaFormat audio = audioFormat;
        List<SampleRing.Sample> audioSamples =
            audio == null ? new ArrayList<>() : audioRing.snapshot(baseUs, toUs, false);

        ContentResolver resolver = getContentResolver();
        String name = "RePic_" + new SimpleDateFormat("yyyy-MM-dd_HH-mm-ss", Locale.US).format(new Date()) + ".mp4";
        ContentValues values = new ContentValues();
        values.put(MediaStore.Video.Media.DISPLAY_NAME, name);
        values.put(MediaStore.Video.Media.MIME_TYPE, "video/mp4");
        values.put(MediaStore.Video.Media.RELATIVE_PATH, Environment.DIRECTORY_MOVIES + "/RePic");
        values.put(MediaStore.Video.Media.IS_PENDING, 1);
        Uri uri = resolver.insert(MediaStore.Video.Media.EXTERNAL_CONTENT_URI, values);
        if (uri == null) {
            throw new IOException("não consegui criar o arquivo");
        }
        boolean done = false;
        try (ParcelFileDescriptor fd = resolver.openFileDescriptor(uri, "w")) {
            if (fd == null) {
                throw new IOException("não consegui abrir o arquivo");
            }
            mux(fd, video, audio, videoSamples, audioSamples, baseUs);
            values.clear();
            values.put(MediaStore.Video.Media.IS_PENDING, 0);
            resolver.update(uri, values, null, null);
            done = true;
            return uri;
        } finally {
            if (!done) {
                resolver.delete(uri, null, null);
            }
        }
    }

    private void mux(
        ParcelFileDescriptor fd,
        MediaFormat videoFmt,
        MediaFormat audioFmt,
        List<SampleRing.Sample> videoSamples,
        List<SampleRing.Sample> audioSamples,
        long baseUs
    ) throws IOException {
        MediaMuxer muxer = new MediaMuxer(fd.getFileDescriptor(), MediaMuxer.OutputFormat.MUXER_OUTPUT_MPEG_4);
        ByteBuffer buffer = ByteBuffer.allocateDirect(4 * 1024 * 1024);
        MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
        boolean started = false;
        try (
            SampleRing.Readers videoFiles = new SampleRing.Readers();
            SampleRing.Readers audioFiles = new SampleRing.Readers()
        ) {
            int videoTrack = muxer.addTrack(videoFmt);
            int audioTrack = audioFmt == null || audioSamples.isEmpty() ? -1 : muxer.addTrack(audioFmt);
            muxer.start();
            started = true;
            int v = 0;
            int a = 0;
            // Intercala por tempo, como um player espera ler.
            while (v < videoSamples.size() || (audioTrack >= 0 && a < audioSamples.size())) {
                boolean takeVideo =
                    audioTrack < 0 ||
                    a >= audioSamples.size() ||
                    (v < videoSamples.size() && videoSamples.get(v).ptsUs <= audioSamples.get(a).ptsUs);
                SampleRing.Sample s = takeVideo ? videoSamples.get(v++) : audioSamples.get(a++);
                if (s.size > buffer.capacity()) {
                    buffer = ByteBuffer.allocateDirect(s.size * 2);
                }
                if (takeVideo) {
                    videoRing.read(s, buffer, videoFiles);
                } else {
                    audioRing.read(s, buffer, audioFiles);
                }
                info.set(0, s.size, Math.max(0, s.ptsUs - baseUs), s.flags);
                muxer.writeSampleData(takeVideo ? videoTrack : audioTrack, buffer, info);
            }
        } finally {
            if (started) {
                muxer.stop();
            }
            muxer.release();
        }
    }

    // --- Notificação e bolha --------------------------------------------

    private void createChannel() {
        NotificationChannel channel = new NotificationChannel(
            CHANNEL,
            getString(R.string.channel_name),
            NotificationManager.IMPORTANCE_LOW
        );
        getSystemService(NotificationManager.class).createNotificationChannel(channel);
    }

    private PendingIntent serviceAction(String action) {
        Intent intent = new Intent(this, CaptureService.class).setAction(action);
        return PendingIntent.getService(this, action.hashCode(), intent, PendingIntent.FLAG_IMMUTABLE);
    }

    private Notification buildNotification() {
        Intent open = new Intent(this, MainActivity.class);
        PendingIntent openApp = PendingIntent.getActivity(this, 0, open, PendingIntent.FLAG_IMMUTABLE);
        return new Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_repic)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(getString(R.string.recording_loop))
            .setContentIntent(openApp)
            .setOngoing(true)
            .addAction(new Notification.Action.Builder(null, getString(R.string.clip), serviceAction(ACTION_CLIP)).build())
            .addAction(new Notification.Action.Builder(null, getString(R.string.record), serviceAction(ACTION_RECORD)).build())
            .addAction(new Notification.Action.Builder(null, getString(R.string.stop), serviceAction(ACTION_STOP)).build())
            .build();
    }

    private void startForegroundNow() {
        int type = ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION;
        // Só declara microfone com a permissão dada: sem ela o Android derruba o serviço.
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
            type |= ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE;
        }
        startForeground(NOTIFICATION_ID, buildNotification(), type);
    }

    private void updateBubbleVisibility() {
        boolean want = instance != null && Settings.canDrawOverlays(this);
        if (want && bubble == null) {
            addBubble();
        } else if (!want && bubble != null) {
            removeBubble();
        }
    }

    /**
     * Bolinha redonda que dá pra arrastar. Um toque abre as opções embaixo dela;
     * fechada, ela ocupa o mínimo possível da tela (e do vídeo).
     */
    @SuppressLint("ClickableViewAccessibility")
    private void addBubble() {
        float density = getResources().getDisplayMetrics().density;
        int pad = (int) (8 * density);
        int size = (int) (52 * density);

        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);

        bubbleRound = new TextView(this);
        bubbleRound.setGravity(Gravity.CENTER);
        bubbleRound.setTextColor(Color.WHITE);
        bubbleRound.setTextSize(11);
        root.addView(bubbleRound, new LinearLayout.LayoutParams(size, size));

        LinearLayout panel = new LinearLayout(this);
        panel.setOrientation(LinearLayout.VERTICAL);
        panel.setPadding(pad, pad, pad, pad);
        GradientDrawable background = new GradientDrawable();
        background.setColor(Color.argb(235, 24, 24, 28));
        background.setCornerRadius(12 * density);
        panel.setBackground(background);
        panel.setVisibility(View.GONE);

        bubbleStatus = new TextView(this);
        bubbleStatus.setTextColor(Color.WHITE);
        bubbleStatus.setTextSize(12);
        bubbleStatus.setPadding(0, 0, 0, pad / 2);
        panel.addView(bubbleStatus);

        LinearLayout buttons = new LinearLayout(this);
        TextView clip = bubbleButton(option(R.string.clip, R.string.clip_hint), Color.rgb(235, 64, 64), pad);
        clip.setOnClickListener(v -> {
            closePanel();
            control.post(this::saveClip);
        });
        bubbleRecord = bubbleButton(option(R.string.record, R.string.record_hint), Color.rgb(60, 60, 68), pad);
        bubbleRecord.setOnClickListener(v -> {
            closePanel();
            control.post(this::toggleRecording);
        });
        buttons.addView(clip);
        buttons.addView(new View(this), new LinearLayout.LayoutParams(pad / 2, 1));
        buttons.addView(bubbleRecord);
        panel.addView(buttons);
        LinearLayout.LayoutParams panelGap = new LinearLayout.LayoutParams(
            LinearLayout.LayoutParams.WRAP_CONTENT,
            LinearLayout.LayoutParams.WRAP_CONTENT
        );
        panelGap.topMargin = pad / 2;
        root.addView(panel, panelGap);
        bubblePanel = panel;

        WindowManager.LayoutParams params = new WindowManager.LayoutParams(
            WindowManager.LayoutParams.WRAP_CONTENT,
            WindowManager.LayoutParams.WRAP_CONTENT,
            WindowManager.LayoutParams.TYPE_APPLICATION_OVERLAY,
            // Sem foco, pro jogo continuar recebendo o toque; o "fora" fecha as opções.
            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE | WindowManager.LayoutParams.FLAG_WATCH_OUTSIDE_TOUCH,
            PixelFormat.TRANSLUCENT
        );
        params.gravity = Gravity.TOP | Gravity.START;
        // Começa na metade da tela, longe dos botões de cima do app.
        params.x = (int) (12 * density);
        params.y = getResources().getDisplayMetrics().heightPixels / 2;
        bubbleParams = params;

        WindowManager wm = getSystemService(WindowManager.class);
        int slop = ViewConfiguration.get(this).getScaledTouchSlop();
        root.setOnTouchListener((view, event) -> {
            if (event.getAction() == MotionEvent.ACTION_OUTSIDE) {
                closePanel();
            }
            return false;
        });
        bubbleRound.setOnTouchListener(new View.OnTouchListener() {
            private int startX;
            private int startY;
            private float touchX;
            private float touchY;
            private boolean dragging;

            @Override
            public boolean onTouch(View view, MotionEvent event) {
                switch (event.getAction()) {
                    case MotionEvent.ACTION_DOWN:
                        startX = params.x;
                        startY = params.y;
                        touchX = event.getRawX();
                        touchY = event.getRawY();
                        dragging = false;
                        return true;
                    case MotionEvent.ACTION_MOVE:
                        float dx = event.getRawX() - touchX;
                        float dy = event.getRawY() - touchY;
                        if (dragging || Math.abs(dx) > slop || Math.abs(dy) > slop) {
                            dragging = true;
                            closePanel();
                            params.x = startX + (int) dx;
                            params.y = startY + (int) dy;
                            wm.updateViewLayout(root, params);
                        }
                        return true;
                    case MotionEvent.ACTION_UP:
                        if (!dragging) {
                            togglePanel();
                        }
                        return true;
                    default:
                        return false;
                }
            }
        });
        try {
            wm.addView(root, params);
            bubble = root;
            updateBubble();
        } catch (RuntimeException e) {
            log("bolha: não abriu: " + e);
        }
    }

    private void togglePanel() {
        if (bubblePanel.getVisibility() == View.VISIBLE) {
            closePanel();
            return;
        }
        updateBubble();
        bubblePanel.setVisibility(View.VISIBLE);
        // Perto da borda, as opções cairiam pra fora: empurra enquanto estão abertas.
        bubble.measure(View.MeasureSpec.UNSPECIFIED, View.MeasureSpec.UNSPECIFIED);
        DisplayMetrics metrics = getResources().getDisplayMetrics();
        bubbleHomeX = bubbleParams.x;
        bubbleHomeY = bubbleParams.y;
        bubbleParams.x = Math.max(0, Math.min(bubbleParams.x, metrics.widthPixels - bubble.getMeasuredWidth()));
        bubbleParams.y = Math.max(0, Math.min(bubbleParams.y, metrics.heightPixels - bubble.getMeasuredHeight()));
        getSystemService(WindowManager.class).updateViewLayout(bubble, bubbleParams);
    }

    private void closePanel() {
        if (bubblePanel == null || bubblePanel.getVisibility() != View.VISIBLE) {
            return;
        }
        bubblePanel.setVisibility(View.GONE);
        bubbleParams.x = bubbleHomeX;
        bubbleParams.y = bubbleHomeY;
        getSystemService(WindowManager.class).updateViewLayout(bubble, bubbleParams);
    }

    /** Nome da opção e, menor embaixo, o que ela faz. */
    private CharSequence option(int title, int hint) {
        String name = getString(title);
        SpannableString text = new SpannableString(name + "\n" + getString(hint));
        text.setSpan(new RelativeSizeSpan(0.75f), name.length(), text.length(), 0);
        return text;
    }

    private TextView bubbleButton(CharSequence text, int color, int pad) {
        TextView button = new TextView(this);
        button.setText(text);
        button.setTextColor(Color.WHITE);
        button.setTextSize(14);
        button.setGravity(Gravity.CENTER);
        button.setPadding(pad * 2, pad + pad / 2, pad * 2, pad + pad / 2);
        GradientDrawable shape = new GradientDrawable();
        shape.setColor(color);
        shape.setCornerRadius(pad);
        button.setBackground(shape);
        return button;
    }

    private void removeBubble() {
        if (bubble != null) {
            try {
                getSystemService(WindowManager.class).removeView(bubble);
            } catch (RuntimeException ignored) {
                // A janela já tinha saído.
            }
            bubble = null;
            bubblePanel = null;
        }
    }

    private void updateBubble() {
        if (bubble == null) {
            return;
        }
        GradientDrawable circle = new GradientDrawable();
        circle.setShape(GradientDrawable.OVAL);
        if (recording) {
            long secs = recordingSeconds();
            String clock = String.format(Locale.US, "%02d:%02d", secs / 60, secs % 60);
            String limit = config.limitMb > 0 ? " / " + formatSize(config.limitMb * 1_000_000L) : "";
            circle.setColor(Color.rgb(235, 64, 64));
            bubbleRound.setBackground(circle);
            bubbleRound.setText(clock);
            bubbleStatus.setText(clock + " · " + formatSize(recBytes) + limit);
            bubbleStatus.setVisibility(View.VISIBLE);
            bubbleRecord.setText(R.string.stop);
        } else {
            // A mesma bolinha de REC do ícone do app.
            circle.setColor(Color.argb(235, 24, 24, 28));
            circle.setStroke((int) (2 * getResources().getDisplayMetrics().density), Color.rgb(245, 245, 245));
            GradientDrawable dot = new GradientDrawable();
            dot.setShape(GradientDrawable.OVAL);
            dot.setColor(saving ? Color.rgb(150, 150, 160) : Color.rgb(235, 64, 64));
            LayerDrawable icon = new LayerDrawable(new Drawable[] { circle, dot });
            int inset = (int) (15 * getResources().getDisplayMetrics().density);
            icon.setLayerInset(1, inset, inset, inset, inset);
            bubbleRound.setBackground(icon);
            bubbleRound.setText("");
            bubbleStatus.setVisibility(View.GONE);
            bubbleRecord.setText(option(R.string.record, R.string.record_hint));
        }
    }

    static String formatSize(long bytes) {
        double mb = bytes / 1_000_000.0;
        if (mb >= 1000) {
            return String.format(Locale.US, "%.1f GB", mb / 1000);
        }
        return String.format(Locale.US, mb < 10 ? "%.1f MB" : "%.0f MB", mb);
    }

    // --- Utilidades -----------------------------------------------------

    private void message(String text) {
        lastMessage = text;
        main.post(() -> Toast.makeText(this, text, Toast.LENGTH_SHORT).show());
    }

    private static void sleep(long ms) {
        try {
            Thread.sleep(ms);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
    }

    private static void join(Thread thread) {
        if (thread != null) {
            try {
                thread.join(3000);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        }
    }

    /** Diário pra investigar problemas que não dá pra reproduzir. */
    private void log(String text) {
        Log.i(TAG, text);
        File file = new File(getFilesDir(), "repic.log");
        boolean append = file.length() < 512 * 1024;
        try (FileWriter writer = new FileWriter(file, append)) {
            String time = new SimpleDateFormat("HH:mm:ss.SSS", Locale.US).format(new Date());
            writer.write(time + " " + text + "\n");
        } catch (IOException ignored) {
            // Sem diário, o app continua.
        }
    }

    static void send(Context context, String action) {
        context.startService(new Intent(context, CaptureService.class).setAction(action));
    }
}
