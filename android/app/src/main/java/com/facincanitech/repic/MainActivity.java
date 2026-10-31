package com.facincanitech.repic;

import android.os.Bundle;
import com.getcapacitor.BridgeActivity;

public class MainActivity extends BridgeActivity {

    @Override
    public void onCreate(Bundle savedInstanceState) {
        registerPlugin(RePicPlugin.class);
        super.onCreate(savedInstanceState);
    }
}
