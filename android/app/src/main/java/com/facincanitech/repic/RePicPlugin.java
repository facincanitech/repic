package com.facincanitech.repic;

import android.Manifest;
import android.app.Activity;
import android.content.ContentUris;
import android.content.Intent;
import android.database.Cursor;
import android.graphics.Bitmap;
import android.media.MediaMetadataRetriever;
import android.media.projection.MediaProjectionManager;
import android.net.Uri;
import android.os.Build;
import android.provider.MediaStore;
import android.provider.Settings;
import android.util.Base64;
import android.util.Log;
import android.util.Size;
import androidx.activity.result.ActivityResult;
import com.getcapacitor.JSArray;
import com.getcapacitor.JSObject;
import com.getcapacitor.PermissionState;
import com.getcapacitor.Plugin;
import com.getcapacitor.PluginCall;
import com.getcapacitor.PluginMethod;
import com.getcapacitor.annotation.ActivityCallback;
import com.getcapacitor.annotation.CapacitorPlugin;
import com.getcapacitor.annotation.Permission;
import com.getcapacitor.annotation.PermissionCallback;
import java.io.ByteArrayOutputStream;
import java.io.IOException;

/** Ponte entre a interface (que vem do site) e a captura nativa. */
@CapacitorPlugin(
    name = "RePic",
    permissions = {
        @Permission(alias = "audio", strings = { Manifest.permission.RECORD_AUDIO }),
        @Permission(alias = "notifications", strings = { Manifest.permission.POST_NOTIFICATIONS }),
    }
)
public class RePicPlugin extends Plugin {

    @PluginMethod
    public void getState(PluginCall call) {
        CaptureService service = CaptureService.running();
        JSObject out = new JSObject();
        out.put("running", service != null);
        out.put("recording", service != null && service.isRecording());
        out.put("saving", service != null && service.isSaving());
        out.put("recordingSeconds", service == null ? 0 : service.recordingSeconds());
        out.put("recordingBytes", service == null ? 0 : service.recordingBytes());
        out.put("videoSize", service == null ? "" : service.videoSize());
        out.put("message", service == null ? "" : service.lastMessage());
        out.put("canOverlay", Settings.canDrawOverlays(getContext()));
        out.put("nativeVersion", BuildConfig.VERSION_NAME);
        call.resolve(out);
    }

    @PluginMethod
    public void getConfig(PluginCall call) {
        RePicConfig c = RePicConfig.load(getContext());
        JSObject out = new JSObject();
        out.put("seconds", c.seconds);
        out.put("fps", c.fps);
        out.put("resolution", c.resolution);
        out.put("quality", c.quality);
        out.put("audio", c.audio);
        out.put("mic", c.mic);
        out.put("limitMb", c.limitMb);
        call.resolve(out);
    }

    @PluginMethod
    public void setConfig(PluginCall call) {
        RePicConfig c = RePicConfig.load(getContext());
        c.seconds = call.getInt("seconds", c.seconds);
        c.fps = call.getInt("fps", c.fps);
        c.resolution = call.getInt("resolution", c.resolution);
        c.quality = call.getInt("quality", c.quality);
        c.audio = call.getBoolean("audio", c.audio);
        c.mic = call.getBoolean("mic", c.mic);
        c.limitMb = call.getInt("limitMb", c.limitMb);
        c.save(getContext());
        if (CaptureService.running() != null) {
            CaptureService.send(getContext(), CaptureService.ACTION_RECONFIGURE);
        }
        call.resolve();
    }

    /** Pede as permissões e, depois, a confirmação de captura de tela do Android. */
    @PluginMethod
    public void startCapture(PluginCall call) {
        if (getPermissionState("audio") != PermissionState.GRANTED) {
            String[] aliases = Build.VERSION.SDK_INT >= 33
                ? new String[] { "audio", "notifications" }
                : new String[] { "audio" };
            requestPermissionForAliases(aliases, call, "onPermissions");
        } else {
            askProjection(call);
        }
    }

    @PermissionCallback
    private void onPermissions(PluginCall call) {
        // Sem som ainda dá pra gravar o vídeo: segue, e a interface mostra o estado.
        askProjection(call);
    }

    private void askProjection(PluginCall call) {
        MediaProjectionManager manager = getContext().getSystemService(MediaProjectionManager.class);
        startActivityForResult(call, manager.createScreenCaptureIntent(), "onProjection");
    }

    @ActivityCallback
    private void onProjection(PluginCall call, ActivityResult result) {
        if (call == null) {
            return;
        }
        if (result.getResultCode() != Activity.RESULT_OK || result.getData() == null) {
            call.reject("cancelled");
            return;
        }
        Intent intent = new Intent(getContext(), CaptureService.class)
            .setAction(CaptureService.ACTION_START)
            .putExtra(CaptureService.EXTRA_RESULT, result.getResultCode())
            .putExtra(CaptureService.EXTRA_DATA, result.getData());
        getContext().startForegroundService(intent);
        call.resolve();
    }

    @PluginMethod
    public void stopCapture(PluginCall call) {
        sendIfRunning(CaptureService.ACTION_STOP);
        call.resolve();
    }

    @PluginMethod
    public void clip(PluginCall call) {
        sendIfRunning(CaptureService.ACTION_CLIP);
        call.resolve();
    }

    @PluginMethod
    public void toggleRecording(PluginCall call) {
        sendIfRunning(CaptureService.ACTION_RECORD);
        call.resolve();
    }

    private void sendIfRunning(String action) {
        if (CaptureService.running() != null) {
            CaptureService.send(getContext(), action);
        }
    }

    /** Abre a tela do Android que libera a bolha sobre os outros apps. */
    @PluginMethod
    public void requestOverlay(PluginCall call) {
        Intent intent = new Intent(
            Settings.ACTION_MANAGE_OVERLAY_PERMISSION,
            Uri.parse("package:" + getContext().getPackageName())
        );
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        getContext().startActivity(intent);
        call.resolve();
    }

    /** Clipes do próprio app em Movies/RePic, do mais novo pro mais velho. */
    @PluginMethod
    public void listClips(PluginCall call) {
        JSArray clips = new JSArray();
        String[] columns = {
            MediaStore.Video.Media._ID,
            MediaStore.Video.Media.DISPLAY_NAME,
            MediaStore.Video.Media.SIZE,
            MediaStore.Video.Media.DATE_ADDED,
            MediaStore.Video.Media.DURATION,
        };
        try (
            Cursor cursor = getContext()
                .getContentResolver()
                .query(
                    MediaStore.Video.Media.EXTERNAL_CONTENT_URI,
                    columns,
                    // Sem o ESCAPE, a barra vira um caractere comum e nada bate.
                    MediaStore.Video.Media.DISPLAY_NAME + " LIKE ? ESCAPE '\\'",
                    new String[] { "RePic\\_%" },
                    MediaStore.Video.Media.DATE_ADDED + " DESC"
                )
        ) {
            while (cursor != null && cursor.moveToNext()) {
                JSObject clip = new JSObject();
                clip.put("id", cursor.getLong(0));
                clip.put("name", cursor.getString(1));
                clip.put("size", cursor.getLong(2));
                clip.put("date", cursor.getLong(3));
                clip.put("duration", cursor.getLong(4));
                clips.put(clip);
            }
        }
        JSObject out = new JSObject();
        out.put("clips", clips);
        call.resolve(out);
    }

    @PluginMethod
    public void thumbnail(PluginCall call) {
        Uri uri = clipUri(call);
        if (uri == null) {
            call.reject("clipe inválido");
            return;
        }
        // Fora da thread principal: a lista carrega várias miniaturas.
        getBridge()
            .execute(() -> {
                try {
                    ByteArrayOutputStream bytes = new ByteArrayOutputStream();
                    loadFrame(uri).compress(Bitmap.CompressFormat.JPEG, 80, bytes);
                    JSObject out = new JSObject();
                    out.put("image", "data:image/jpeg;base64," + Base64.encodeToString(bytes.toByteArray(), Base64.NO_WRAP));
                    call.resolve(out);
                } catch (Exception e) {
                    Log.w("RePic", "miniatura: falhou: " + e);
                    call.reject("sem miniatura");
                }
            });
    }

    /** A miniatura do sistema; se ele não tiver, tira um quadro do próprio vídeo. */
    private Bitmap loadFrame(Uri uri) throws IOException {
        try {
            return getContext().getContentResolver().loadThumbnail(uri, new Size(360, 480), null);
        } catch (IOException | RuntimeException e) {
            Log.w("RePic", "miniatura do sistema: falhou: " + e);
        }
        try (MediaMetadataRetriever retriever = new MediaMetadataRetriever()) {
            retriever.setDataSource(getContext(), uri);
            Bitmap frame = retriever.getFrameAtTime(0, MediaMetadataRetriever.OPTION_CLOSEST_SYNC);
            if (frame == null) {
                throw new IOException("vídeo sem quadro");
            }
            float scale = Math.min(1f, 480f / Math.max(frame.getWidth(), frame.getHeight()));
            return Bitmap.createScaledBitmap(frame, Math.round(frame.getWidth() * scale), Math.round(frame.getHeight() * scale), true);
        }
    }

    @PluginMethod
    public void openClip(PluginCall call) {
        Uri uri = clipUri(call);
        if (uri == null) {
            call.reject("clipe inválido");
            return;
        }
        Intent intent = new Intent(Intent.ACTION_VIEW)
            .setDataAndType(uri, "video/mp4")
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        getActivity().startActivity(Intent.createChooser(intent, null));
        call.resolve();
    }

    @PluginMethod
    public void shareClip(PluginCall call) {
        Uri uri = clipUri(call);
        if (uri == null) {
            call.reject("clipe inválido");
            return;
        }
        Intent intent = new Intent(Intent.ACTION_SEND)
            .setType("video/mp4")
            .putExtra(Intent.EXTRA_STREAM, uri)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        getActivity().startActivity(Intent.createChooser(intent, null));
        call.resolve();
    }

    @PluginMethod
    public void deleteClip(PluginCall call) {
        Uri uri = clipUri(call);
        if (uri == null) {
            call.reject("clipe inválido");
            return;
        }
        try {
            getContext().getContentResolver().delete(uri, null, null);
            call.resolve();
        } catch (RuntimeException e) {
            // Clipe de antes de reinstalar o app: o Android não deixa apagar sem confirmar.
            call.reject("não deu pra apagar");
        }
    }

    private Uri clipUri(PluginCall call) {
        // getLong devolve null quando o número cabe em int: o JSON o lê como Integer.
        long id = call.getData().optLong("id", -1);
        return id < 0 ? null : ContentUris.withAppendedId(MediaStore.Video.Media.EXTERNAL_CONTENT_URI, id);
    }
}
